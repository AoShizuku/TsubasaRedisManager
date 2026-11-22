use crate::error::{AppError, AppResult};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const DRAIN_GRACE_TIMEOUT_MS: u64 = 100;
const KILL_WAIT_GRACE_TIMEOUT_MS: u64 = 100;
const ABORT_JOIN_GRACE_TIMEOUT_MS: u64 = 10;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormatterRunOptions {
    pub command: String,
    pub params: Option<String>,
    pub value_base64: String,
    pub key: Option<String>,
    pub field: Option<String>,
    pub score: Option<String>,
    pub member: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub working_dir: Option<String>,
    pub allow_shell: Option<bool>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FormatterRunResult {
    pub stdout: String,
    pub stderr: String,
    pub status_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
    pub preview_command: String,
}

#[derive(Debug)]
struct PreparedInvocation {
    command: String,
    args: Vec<String>,
    preview_command: String,
    hex_file: Option<NamedTempFile>,
}

#[derive(Debug)]
struct NamedTempFile {
    path: PathBuf,
    file: Option<File>,
}

impl NamedTempFile {
    fn new() -> io::Result<Self> {
        let temp_dir = std::env::temp_dir();
        let process_id = std::process::id();

        for _ in 0..100 {
            let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = temp_dir.join(format!(
                "rewrite-redis-formatter-{process_id}-{nanos}-{counter}.hex"
            ));

            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);

            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create unique formatter temporary file",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Write for NamedTempFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file
            .as_mut()
            .expect("temporary file should be open")
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .expect("temporary file should be open")
            .flush()
    }
}

impl Drop for NamedTempFile {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

pub async fn run_formatter(options: FormatterRunOptions) -> AppResult<FormatterRunResult> {
    let prepared = prepare_invocation(&options)?;
    let working_dir = resolve_working_dir(options.working_dir.as_deref())?;
    let timeout_ms = effective_timeout_ms(options.timeout_ms);
    let max_output_bytes = effective_max_output_bytes(options.max_output_bytes);
    let allow_shell = options.allow_shell.unwrap_or(false);

    execute_prepared(
        prepared,
        working_dir,
        Duration::from_millis(timeout_ms),
        max_output_bytes,
        allow_shell,
    )
    .await
}

fn effective_timeout_ms(value: Option<u64>) -> u64 {
    value.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1, MAX_TIMEOUT_MS)
}

fn effective_max_output_bytes(value: Option<usize>) -> usize {
    value
        .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES)
        .clamp(1, MAX_OUTPUT_BYTES)
}

fn prepare_invocation(options: &FormatterRunOptions) -> AppResult<PreparedInvocation> {
    let command = options.command.trim().to_string();
    if command.is_empty() {
        return Err(formatter_error(
            "formatter.command_missing",
            "Formatter command is missing",
            Some("command"),
        ));
    }
    if options.allow_shell != Some(true)
        && command.split_whitespace().count() > 1
        && !Path::new(&command).is_file()
    {
        return Err(formatter_error(
            "formatter.command_missing",
            "Formatter command must be an executable path when shell execution is disabled",
            Some(&command),
        ));
    }

    let value = base64::engine::general_purpose::STANDARD
        .decode(&options.value_base64)
        .map_err(|error| {
            formatter_error(
                "formatter.template_invalid",
                "Formatter value is not valid base64",
                Some(&error.to_string()),
            )
        })?;
    let value_text = String::from_utf8_lossy(&value).to_string();
    let hex = encode_hex(&value);
    let params = options.params.as_deref().unwrap_or("");
    let mut hex_file = None;
    let hex_file_path = if params.contains("{HEX_FILE}") {
        let mut file = NamedTempFile::new().map_err(|error| {
            formatter_error(
                "formatter.io_failed",
                "Formatter temporary file could not be created",
                Some(&error.to_string()),
            )
        })?;
        file.write_all(hex.as_bytes()).map_err(|error| {
            formatter_error(
                "formatter.io_failed",
                "Formatter temporary file could not be written",
                Some(&error.to_string()),
            )
        })?;
        let path = file.path().to_string_lossy().to_string();
        hex_file = Some(file);
        path
    } else {
        String::new()
    };

    let replacements = [
        ("{VALUE}", value_text.as_str()),
        ("{HEX}", hex.as_str()),
        ("{HEX_FILE}", hex_file_path.as_str()),
        ("{KEY}", options.key.as_deref().unwrap_or("")),
        ("{FIELD}", options.field.as_deref().unwrap_or("")),
        ("{SCORE}", options.score.as_deref().unwrap_or("")),
        ("{MEMBER}", options.member.as_deref().unwrap_or("")),
    ];

    let arg_templates = shell_words::split(params).map_err(|error| {
        formatter_error(
            "formatter.template_invalid",
            "Formatter params template could not be parsed",
            Some(&error.to_string()),
        )
    })?;
    let args = arg_templates
        .iter()
        .map(|arg| render_template(arg, &replacements))
        .collect::<Vec<_>>();
    let preview_command =
        shell_words::join(std::iter::once(command.as_str()).chain(args.iter().map(String::as_str)));

    Ok(PreparedInvocation {
        command,
        args,
        preview_command,
        hex_file,
    })
}

async fn execute_prepared(
    prepared: PreparedInvocation,
    working_dir: PathBuf,
    timeout: Duration,
    max_output_bytes: usize,
    allow_shell: bool,
) -> AppResult<FormatterRunResult> {
    let PreparedInvocation {
        command,
        args,
        preview_command,
        hex_file,
    } = prepared;
    let _hex_file = hex_file;
    let mut command = build_process_command(&command, &args, allow_shell)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(working_dir)
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|error| {
        formatter_error(
            "formatter.spawn_failed",
            "Formatter command could not be started",
            Some(&error.to_string()),
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        formatter_error(
            "formatter.io_failed",
            "Formatter stdout pipe could not be captured",
            None,
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        formatter_error(
            "formatter.io_failed",
            "Formatter stderr pipe could not be captured",
            None,
        )
    })?;
    let budget = Arc::new(Mutex::new(max_output_bytes));
    let stdout_output = Arc::new(Mutex::new(CapturedOutput::default()));
    let stderr_output = Arc::new(Mutex::new(CapturedOutput::default()));
    let stdout_task = tokio::spawn(read_limited(
        stdout,
        Arc::clone(&budget),
        Arc::clone(&stdout_output),
    ));
    let stderr_task = tokio::spawn(read_limited(stderr, budget, Arc::clone(&stderr_output)));

    let (mut status_code, mut timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => {
            let status = result.map_err(|error| {
                formatter_error(
                    "formatter.io_failed",
                    "Formatter command could not be waited on",
                    Some(&error.to_string()),
                )
            })?;
            (status.code(), false)
        }
        Err(_) => {
            // Timeout cleanup is deliberately bounded to the direct child process. Descendant
            // process tree cleanup needs platform-specific process groups/job objects.
            let _ = child.start_kill();
            let _ = tokio::time::timeout(
                Duration::from_millis(KILL_WAIT_GRACE_TIMEOUT_MS),
                child.wait(),
            )
            .await;
            (None, true)
        }
    };

    let drain_timeout = Duration::from_millis(DRAIN_GRACE_TIMEOUT_MS);
    let (stdout, stdout_aborted) =
        join_captured_output_with_timeout(stdout_task, stdout_output, drain_timeout).await?;
    let (stderr, stderr_aborted) =
        join_captured_output_with_timeout(stderr_task, stderr_output, drain_timeout).await?;
    if stdout_aborted || stderr_aborted {
        timed_out = true;
        status_code = None;
    }
    let truncated = stdout.truncated || stderr.truncated || stdout_aborted || stderr_aborted;

    Ok(FormatterRunResult {
        stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
        status_code,
        timed_out,
        truncated,
        preview_command,
    })
}

fn build_process_command(command: &str, args: &[String], allow_shell: bool) -> AppResult<Command> {
    if allow_shell && args.is_empty() {
        return raw_shell_command(command);
    }

    let executable = resolve_executable(command)?;
    let mut process = Command::new(executable);
    process.args(args);
    Ok(process)
}

#[cfg(windows)]
fn raw_shell_command(command_line: &str) -> AppResult<Command> {
    let mut command = Command::new(resolve_system_shell()?);
    command.raw_arg(format!("/C {command_line}"));
    Ok(command)
}

#[cfg(not(windows))]
fn raw_shell_command(command_line: &str) -> AppResult<Command> {
    let mut command = Command::new(resolve_system_shell()?);
    command.arg("-c").arg(command_line);
    Ok(command)
}

#[cfg(windows)]
fn resolve_system_shell() -> AppResult<PathBuf> {
    if let Some(system_directory) = windows_system_directory() {
        let candidate = system_directory.join("cmd.exe");
        if let Some(path) = canonical_file(&candidate) {
            return Ok(path);
        }
    }

    Err(formatter_error(
        "formatter.command_missing",
        "System command shell could not be found",
        Some("cmd.exe"),
    ))
}

#[cfg(windows)]
fn windows_system_directory() -> Option<PathBuf> {
    let mut buffer = vec![0u16; 260];

    loop {
        let length = unsafe {
            windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
                buffer.as_mut_ptr(),
                buffer.len() as u32,
            )
        };
        if length == 0 {
            return None;
        }

        let length = length as usize;
        if length < buffer.len() {
            return Some(PathBuf::from(String::from_utf16_lossy(&buffer[..length])));
        }

        buffer.resize(length + 1, 0);
    }
}

#[cfg(not(windows))]
fn resolve_system_shell() -> AppResult<PathBuf> {
    canonical_file("/bin/sh").ok_or_else(|| {
        formatter_error(
            "formatter.command_missing",
            "System command shell could not be found",
            Some("/bin/sh"),
        )
    })
}

fn resolve_executable(command: &str) -> AppResult<PathBuf> {
    let path = Path::new(command);
    if is_rejected_structured_executable(path) {
        return Err(rejected_batch_file_error(command));
    }
    if path.is_absolute() || contains_path_separator(command) {
        return canonical_formatter_executable(path).ok_or_else(|| {
            formatter_error(
                "formatter.command_missing",
                "Formatter executable could not be found",
                Some(command),
            )
        });
    }

    find_executable_in_path(command).ok_or_else(|| {
        formatter_error(
            "formatter.command_missing",
            "Formatter executable could not be found in PATH",
            Some(command),
        )
    })
}

fn rejected_batch_file_error(command: &str) -> AppError {
    formatter_error(
        "formatter.command_missing",
        "Windows batch files require raw shell execution",
        Some(command),
    )
}

fn contains_path_separator(command: &str) -> bool {
    command.contains('/') || command.contains('\\')
}

fn canonical_file<P>(path: P) -> Option<PathBuf>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    if !path.is_file() {
        return None;
    }
    path.canonicalize().ok()
}

fn canonical_formatter_executable<P>(path: P) -> Option<PathBuf>
where
    P: AsRef<Path>,
{
    let path = path.as_ref();
    if is_rejected_structured_executable(path) {
        return None;
    }
    canonical_file(path)
}

#[cfg(windows)]
fn is_rejected_structured_executable(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
}

#[cfg(not(windows))]
fn is_rejected_structured_executable(_path: &Path) -> bool {
    false
}

fn find_executable_in_path(command: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for directory in env::split_paths(&path_var) {
        if directory.as_os_str().is_empty()
            || directory == Path::new(".")
            || directory.is_relative()
        {
            continue;
        }

        for candidate_name in executable_candidate_names(command) {
            let candidate = directory.join(candidate_name);
            if let Some(path) = canonical_formatter_executable(candidate) {
                return Some(path);
            }
        }
    }

    None
}

#[cfg(windows)]
fn executable_candidate_names(command: &str) -> Vec<String> {
    if Path::new(command).extension().is_some() {
        if is_rejected_structured_executable(Path::new(command)) {
            return Vec::new();
        }
        return vec![command.to_string()];
    }

    safe_pathext_extensions()
        .into_iter()
        .map(|extension| format!("{command}{extension}"))
        .collect()
}

#[cfg(windows)]
fn safe_pathext_extensions() -> Vec<String> {
    let mut extensions = vec![".EXE".to_string(), ".COM".to_string()];
    if let Ok(pathext) = env::var("PATHEXT") {
        for extension in pathext.split(';') {
            let extension = extension.trim();
            if extension.is_empty()
                || extension.eq_ignore_ascii_case(".bat")
                || extension.eq_ignore_ascii_case("bat")
                || extension.eq_ignore_ascii_case(".cmd")
                || extension.eq_ignore_ascii_case("cmd")
            {
                continue;
            }

            let extension = if extension.starts_with('.') {
                extension.to_string()
            } else {
                format!(".{extension}")
            };
            if !extensions
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&extension))
            {
                extensions.push(extension);
            }
        }
    }
    extensions
}

#[cfg(not(windows))]
fn executable_candidate_names(command: &str) -> Vec<String> {
    vec![command.to_string()]
}

fn resolve_working_dir(working_dir: Option<&str>) -> AppResult<PathBuf> {
    let Some(working_dir) = working_dir else {
        return Ok(std::env::temp_dir());
    };
    let working_dir = working_dir.trim();
    if working_dir.is_empty() {
        return Ok(std::env::temp_dir());
    }

    let resolved = Path::new(working_dir).canonicalize().map_err(|error| {
        formatter_error(
            "formatter.working_dir_invalid",
            "Formatter working directory is invalid",
            Some(&error.to_string()),
        )
    })?;
    if !resolved.is_dir() {
        return Err(formatter_error(
            "formatter.working_dir_invalid",
            "Formatter working directory is not a directory",
            Some(working_dir),
        ));
    }

    Ok(resolved)
}

#[derive(Debug, Clone, Default)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_limited<R>(
    mut reader: R,
    budget: Arc<Mutex<usize>>,
    output: Arc<Mutex<CapturedOutput>>,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0u8; 8 * 1024];

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }

        let mut remaining = budget.lock().await;
        let keep = read.min(*remaining);
        let mut output = output.lock().await;
        if keep > 0 {
            output.bytes.extend_from_slice(&buffer[..keep]);
            *remaining -= keep;
        }
        if keep < read {
            output.truncated = true;
        }
    }

    Ok(())
}

async fn join_captured_output_with_timeout(
    mut task: tokio::task::JoinHandle<io::Result<()>>,
    output: Arc<Mutex<CapturedOutput>>,
    timeout: Duration,
) -> AppResult<(CapturedOutput, bool)> {
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(result) => {
            map_captured_output_join(result)?;
            Ok((output.lock().await.clone(), false))
        }
        Err(_) => {
            task.abort();
            let _ = tokio::time::timeout(
                Duration::from_millis(ABORT_JOIN_GRACE_TIMEOUT_MS),
                &mut task,
            )
            .await;
            let mut output = output.lock().await;
            output.truncated = true;
            Ok((output.clone(), true))
        }
    }
}

fn map_captured_output_join(
    result: Result<io::Result<()>, tokio::task::JoinError>,
) -> AppResult<()> {
    result
        .map_err(|error| {
            formatter_error(
                "formatter.io_failed",
                "Formatter output task could not be joined",
                Some(&error.to_string()),
            )
        })?
        .map_err(|error| {
            formatter_error(
                "formatter.io_failed",
                "Formatter output could not be read",
                Some(&error.to_string()),
            )
        })
}

fn render_template(input: &str, replacements: &[(&str, &str)]) -> String {
    let mut rendered = String::with_capacity(input.len());
    let mut remaining = input;

    while !remaining.is_empty() {
        if let Some((token, value)) = replacements
            .iter()
            .find(|(token, _)| remaining.starts_with(token))
        {
            rendered.push_str(value);
            remaining = &remaining[token.len()..];
        } else {
            let ch = remaining
                .chars()
                .next()
                .expect("remaining input should not be empty");
            rendered.push(ch);
            remaining = &remaining[ch.len_utf8()..];
        }
    }

    rendered
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

pub fn formatter_error(code: &str, message: &str, detail: Option<&str>) -> AppError {
    AppError::FormatterFailed {
        code: code.into(),
        message: message.into(),
        detail: detail.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    #[cfg(windows)]
    use serial_test::serial;

    #[cfg(windows)]
    struct EnvVarGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    #[cfg(windows)]
    impl EnvVarGuard {
        fn set<P>(key: &'static str, value: P) -> Self
        where
            P: AsRef<std::ffi::OsStr>,
        {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    #[cfg(windows)]
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn options(params: &str) -> FormatterRunOptions {
        FormatterRunOptions {
            command: "formatter-bin".into(),
            params: Some(params.into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"hello world"),
            key: Some("redis:key".into()),
            field: Some("hash-field".into()),
            score: Some("42".into()),
            member: Some("zmember".into()),
            timeout_ms: None,
            max_output_bytes: None,
            working_dir: None,
            allow_shell: None,
        }
    }

    #[test]
    fn clamps_formatter_timeout_to_default_minimum_and_hard_cap() {
        assert_eq!(effective_timeout_ms(None), DEFAULT_TIMEOUT_MS);
        assert_eq!(effective_timeout_ms(Some(0)), 1);
        assert_eq!(effective_timeout_ms(Some(u64::MAX)), MAX_TIMEOUT_MS);
    }

    #[test]
    fn clamps_formatter_output_limit_to_default_minimum_and_hard_cap() {
        assert_eq!(effective_max_output_bytes(None), DEFAULT_MAX_OUTPUT_BYTES);
        assert_eq!(effective_max_output_bytes(Some(0)), 1);
        assert_eq!(
            effective_max_output_bytes(Some(usize::MAX)),
            MAX_OUTPUT_BYTES
        );
    }

    #[test]
    fn renders_supported_templates_in_params() {
        let prepared = prepare_invocation(&options(
            "--key {KEY} --field {FIELD} --score {SCORE} --member {MEMBER} --value \"{VALUE}\" --hex {HEX}",
        ))
        .expect("templates should render");

        assert_eq!(prepared.command, "formatter-bin");
        assert_eq!(
            prepared.args,
            vec![
                "--key",
                "redis:key",
                "--field",
                "hash-field",
                "--score",
                "42",
                "--member",
                "zmember",
                "--value",
                "hello world",
                "--hex",
                "68656c6c6f20776f726c64",
            ]
        );
        assert!(prepared.hex_file.is_none());
    }

    #[test]
    fn creates_hex_file_for_hex_file_template_and_removes_it_on_drop() {
        let path = {
            let prepared = prepare_invocation(&options("--hex-file \"{HEX_FILE}\""))
                .expect("hex file template should render");
            let path = prepared
                .hex_file
                .as_ref()
                .expect("hex file should be created")
                .path()
                .to_path_buf();

            assert!(path.exists());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "68656c6c6f20776f726c64"
            );
            assert_eq!(
                prepared.args,
                vec!["--hex-file".to_string(), path.to_string_lossy().to_string()]
            );
            path
        };

        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn creates_hex_file_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let prepared = prepare_invocation(&options("--hex-file \"{HEX_FILE}\""))
            .expect("hex file template should render");
        let path = prepared
            .hex_file
            .as_ref()
            .expect("hex file should be created")
            .path();
        let mode = std::fs::metadata(path)
            .expect("hex file metadata should be readable")
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn rejects_invalid_base64_content() {
        let mut bad = options("{VALUE}");
        bad.value_base64 = "not base64@@".into();

        let err = prepare_invocation(&bad).expect_err("invalid base64 should fail");

        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.template_invalid"
        ));
    }

    #[test]
    fn preserves_literal_tokens_inside_replacement_values() {
        let mut literal = options("--value {VALUE}");
        literal.value_base64 = base64::engine::general_purpose::STANDARD.encode(b"{HEX}");

        let prepared = prepare_invocation(&literal).expect("literal token text should render");

        assert_eq!(prepared.args, vec!["--value", "{HEX}"]);
    }

    #[test]
    fn replacement_values_cannot_add_or_requote_arguments() {
        let mut literal = options("--value \"{VALUE}\"");
        literal.value_base64 = base64::engine::general_purpose::STANDARD.encode(b"x\" --flag \"y");

        let prepared = prepare_invocation(&literal).expect("literal value should render");

        assert_eq!(prepared.args, vec!["--value", "x\" --flag \"y"]);
    }

    #[test]
    fn replacement_values_with_spaces_stay_in_one_argument() {
        let mut literal = options("--value {VALUE}");
        literal.value_base64 =
            base64::engine::general_purpose::STANDARD.encode(b"hello --flag world");

        let prepared = prepare_invocation(&literal).expect("literal value should render");

        assert_eq!(prepared.args, vec!["--value", "hello --flag world"]);
    }

    #[test]
    fn preview_command_quotes_arguments_with_shell_syntax() {
        let mut literal = options("--value {VALUE}");
        literal.value_base64 =
            base64::engine::general_purpose::STANDARD.encode(b"hello --flag world");

        let prepared = prepare_invocation(&literal).expect("literal value should render");
        let preview_args = shell_words::join(prepared.args.iter().map(String::as_str));

        assert_eq!(
            prepared.preview_command,
            format!("formatter-bin {preview_args}")
        );
    }

    #[test]
    fn preview_command_quotes_spaced_command_path() {
        let (dir, command) = copy_rust_exe_to_spaced_path();
        let mut literal = options("--version");
        literal.command = command.to_string_lossy().to_string();

        let prepared = prepare_invocation(&literal).expect("spaced command should prepare");
        let preview_parts =
            shell_words::split(&prepared.preview_command).expect("preview should parse");
        let _ = std::fs::remove_dir_all(dir);

        assert_eq!(preview_parts, vec![prepared.command, "--version".into()]);
    }

    #[test]
    fn rejects_unparseable_params_template_before_rendering() {
        let bad = options("--key \"{KEY}");

        let err = prepare_invocation(&bad).expect_err("unbalanced quote should fail");

        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.template_invalid"
        ));
    }

    fn rust_exe() -> String {
        std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
    }

    fn copy_rust_exe_to_spaced_path() -> (PathBuf, PathBuf) {
        let source = PathBuf::from(rust_exe())
            .canonicalize()
            .expect("cargo executable path should resolve");
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rewrite redis formatter spaced {} {counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("spaced temp dir should be created");
        let file_name = source
            .file_name()
            .expect("cargo executable should have a file name");
        let destination = dir.join(file_name);
        std::fs::copy(&source, &destination).expect("cargo executable should be copied");
        let permissions = std::fs::metadata(&source)
            .expect("source metadata should be readable")
            .permissions();
        std::fs::set_permissions(&destination, permissions)
            .expect("destination permissions should be set");

        (dir, destination)
    }

    #[cfg(windows)]
    fn stdout_stderr_command() -> String {
        "echo formatter stdout && echo formatter stderr 1>&2".into()
    }

    #[cfg(not(windows))]
    fn stdout_stderr_command() -> String {
        "printf 'formatter stdout\\n'; printf 'formatter stderr\\n' >&2".into()
    }

    #[cfg(windows)]
    fn large_stdout_stderr_command() -> String {
        "echo 1234567890 && echo abcdefghij 1>&2".into()
    }

    #[cfg(not(windows))]
    fn large_stdout_stderr_command() -> String {
        "printf '1234567890\\n'; printf 'abcdefghij\\n' >&2".into()
    }

    #[cfg(windows)]
    fn print_working_dir_command() -> String {
        "cd".into()
    }

    #[cfg(not(windows))]
    fn print_working_dir_command() -> String {
        "pwd".into()
    }

    #[cfg(windows)]
    fn sleep_command() -> String {
        "ping -n 3 127.0.0.1 >nul".into()
    }

    #[cfg(not(windows))]
    fn sleep_command() -> String {
        "sleep 2".into()
    }

    #[cfg(windows)]
    fn exits_with_background_pipe_holder_command() -> String {
        "start /B \"\" ping -n 6 127.0.0.1".into()
    }

    #[cfg(not(windows))]
    fn exits_with_background_pipe_holder_command() -> String {
        "sleep 5 &".into()
    }

    #[cfg(windows)]
    fn marker_then_pipe_holder_command() -> String {
        "echo formatter-marker && start /B \"\" ping -n 6 127.0.0.1".into()
    }

    #[cfg(not(windows))]
    fn marker_then_pipe_holder_command() -> String {
        "printf 'formatter-marker\\n'; sleep 5 &".into()
    }

    #[tokio::test]
    async fn runs_formatter_command_and_captures_stdout_stderr_and_status() {
        let result = run_formatter(FormatterRunOptions {
            command: stdout_stderr_command(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: None,
            allow_shell: Some(true),
        })
        .await
        .expect("shell command should run");

        assert!(result.stdout.contains("formatter stdout"));
        assert!(result.stderr.contains("formatter stderr"));
        assert_eq!(result.status_code, Some(0));
        assert!(!result.timed_out);
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn allows_existing_command_path_with_spaces_when_shell_is_disabled() {
        let (dir, command) = copy_rust_exe_to_spaced_path();
        let result = run_formatter(FormatterRunOptions {
            command: command.to_string_lossy().to_string(),
            params: Some("--version".into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: None,
            allow_shell: None,
        })
        .await;
        let _ = std::fs::remove_dir_all(dir);
        let result = result.expect("spaced executable path should run without shell");

        assert!(result.stdout.contains("cargo"));
        assert_eq!(result.status_code, Some(0));
    }

    #[tokio::test]
    async fn runs_existing_command_path_with_spaces_in_structured_shell_mode() {
        let (dir, command) = copy_rust_exe_to_spaced_path();
        let result = run_formatter(FormatterRunOptions {
            command: command.to_string_lossy().to_string(),
            params: Some("--version".into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: None,
            allow_shell: Some(true),
        })
        .await;
        let _ = std::fs::remove_dir_all(dir);
        let result = result.expect("spaced executable path should run in shell mode");

        assert!(
            result.stdout.contains("cargo"),
            "stdout={:?} stderr={:?} status={:?}",
            result.stdout,
            result.stderr,
            result.status_code
        );
        assert_eq!(result.status_code, Some(0));
    }

    #[tokio::test]
    async fn structured_shell_mode_does_not_invoke_shell_for_arguments() {
        let result = run_formatter(FormatterRunOptions {
            command: rust_exe(),
            params: Some("--version \"x\\\" & echo injected & \\\"\"".into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: None,
            allow_shell: Some(true),
        })
        .await
        .expect("structured allow_shell invocation should run directly");

        let combined_output = format!("{}{}", result.stdout, result.stderr);
        assert!(!combined_output.contains("injected"));
        assert!(result.stdout.contains("cargo"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn raw_shell_uses_system_cmd_not_working_dir_cmd() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let working_dir = std::env::temp_dir().join(format!(
            "rewrite-redis-fake-cmd-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&working_dir).expect("fake cmd dir should be created");
        std::fs::copy(
            std::env::current_exe().expect("test executable should resolve"),
            working_dir.join("cmd.exe"),
        )
        .expect("fake cmd should be copied");

        let result = run_formatter(FormatterRunOptions {
            command: "echo shell-ok".into(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: Some(working_dir.to_string_lossy().to_string()),
            allow_shell: Some(true),
        })
        .await;
        let _ = std::fs::remove_dir_all(working_dir);
        let result = result.expect("raw shell should use the system cmd");

        assert!(
            result.stdout.contains("shell-ok"),
            "stdout={:?} stderr={:?} status={:?}",
            result.stdout,
            result.stderr,
            result.status_code
        );
    }

    #[cfg(windows)]
    #[test]
    #[serial]
    fn system_shell_ignores_arbitrary_comspec() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rewrite-redis-fake-comspec-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("fake comspec dir should be created");
        let fake_cmd = dir.join("cmd.exe");
        std::fs::copy(
            std::env::current_exe().expect("test executable should resolve"),
            &fake_cmd,
        )
        .expect("fake comspec should be copied");
        let _comspec_guard = EnvVarGuard::set("COMSPEC", &fake_cmd);

        let resolved = resolve_system_shell().expect("system shell should resolve");
        let fake_cmd = fake_cmd
            .canonicalize()
            .expect("fake cmd should canonicalize");

        let _ = std::fs::remove_dir_all(dir);
        assert_ne!(resolved, fake_cmd);
        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some("cmd.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    #[serial]
    fn system_shell_ignores_arbitrary_systemroot() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rewrite-redis-fake-systemroot-{}-{counter}",
            std::process::id()
        ));
        let fake_system32 = dir.join("System32");
        std::fs::create_dir_all(&fake_system32).expect("fake System32 should be created");
        let fake_cmd = fake_system32.join("cmd.exe");
        std::fs::copy(
            std::env::current_exe().expect("test executable should resolve"),
            &fake_cmd,
        )
        .expect("fake system cmd should be copied");
        let _systemroot_guard = EnvVarGuard::set("SystemRoot", &dir);

        let resolved = resolve_system_shell().expect("system shell should resolve");
        let fake_cmd = fake_cmd
            .canonicalize()
            .expect("fake cmd should canonicalize");

        let _ = std::fs::remove_dir_all(dir);
        assert_ne!(resolved, fake_cmd);
        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some("cmd.exe")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial]
    async fn structured_bare_command_ignores_working_dir_fake_executable() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let working_dir = std::env::temp_dir().join(format!(
            "rewrite-redis-fake-cargo-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&working_dir).expect("fake cargo dir should be created");
        std::fs::copy(
            std::env::current_exe().expect("test executable should resolve"),
            working_dir.join("cargo.exe"),
        )
        .expect("fake cargo should be copied");

        let result = run_formatter(FormatterRunOptions {
            command: "cargo".into(),
            params: Some("--version".into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: Some(working_dir.to_string_lossy().to_string()),
            allow_shell: None,
        })
        .await;
        let _ = std::fs::remove_dir_all(working_dir);
        let result = result.expect("bare executable should resolve from PATH");

        assert!(
            result.stdout.contains("cargo"),
            "stdout={:?} stderr={:?} status={:?}",
            result.stdout,
            result.stderr,
            result.status_code
        );
        assert_eq!(result.status_code, Some(0));
    }

    #[cfg(windows)]
    #[test]
    #[serial]
    fn structured_path_resolution_ignores_cmd_batch_candidates() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rewrite-redis-path-cmd-only-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("cmd-only path dir should be created");
        std::fs::write(dir.join("formatter.cmd"), "@echo should-not-run\r\n")
            .expect("formatter cmd should be written");
        let _path_guard = EnvVarGuard::set("PATH", &dir);

        let result = resolve_executable("formatter");

        let _ = std::fs::remove_dir_all(dir);
        let err = result.expect_err("cmd should not resolve");
        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.command_missing"
        ));
    }

    #[cfg(windows)]
    #[test]
    #[serial]
    fn structured_path_resolution_includes_exe_when_pathext_omits_it() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rewrite-redis-path-exe-only-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("exe-only path dir should be created");
        let executable = dir.join("formatter.exe");
        std::fs::copy(
            std::env::current_exe().expect("test executable should resolve"),
            &executable,
        )
        .expect("formatter exe should be copied");
        let _path_guard = EnvVarGuard::set("PATH", &dir);
        let _pathext_guard = EnvVarGuard::set("PATHEXT", ".BAT;.CMD");

        let resolved = resolve_executable("formatter").expect("exe should resolve");

        let executable = executable
            .canonicalize()
            .expect("formatter exe should canonicalize");
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(resolved, executable);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn structured_mode_rejects_explicit_cmd_batch_path() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rewrite-redis-explicit-cmd-{}-{counter}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("explicit cmd dir should be created");
        let command = dir.join("formatter.cmd");
        std::fs::write(&command, "@echo should-not-run\r\n")
            .expect("formatter cmd should be written");

        let err = run_formatter(FormatterRunOptions {
            command: command.to_string_lossy().to_string(),
            params: Some("--version".into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: None,
            allow_shell: None,
        })
        .await
        .expect_err("structured mode should reject cmd batch files");
        let _ = std::fs::remove_dir_all(dir);

        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.command_missing"
        ));
    }

    #[tokio::test]
    async fn rejects_missing_bare_executable_before_spawn() {
        let err = run_formatter(FormatterRunOptions {
            command: "rewrite-redis-definitely-missing-formatter-executable".into(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: None,
            allow_shell: None,
        })
        .await
        .expect_err("missing executable should be rejected before spawn");

        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.command_missing"
        ));
    }

    #[tokio::test]
    async fn maps_spawn_failure_to_spawn_failed() {
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rewrite-redis-not-executable-{}-{counter}.txt",
            std::process::id()
        ));
        std::fs::write(&path, "not an executable").expect("non-executable file should be written");

        let err = run_formatter(FormatterRunOptions {
            command: path.to_string_lossy().to_string(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: None,
            allow_shell: None,
        })
        .await
        .expect_err("non-executable file should fail to spawn");
        let _ = std::fs::remove_file(path);

        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.spawn_failed"
        ));
    }

    #[tokio::test]
    async fn rejects_missing_working_directory() {
        let missing_dir = std::env::temp_dir().join(format!(
            "rewrite-redis-missing-working-dir-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let err = run_formatter(FormatterRunOptions {
            command: rust_exe(),
            params: Some("--version".into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: None,
            max_output_bytes: None,
            working_dir: Some(missing_dir.to_string_lossy().to_string()),
            allow_shell: None,
        })
        .await
        .expect_err("missing working directory should fail");

        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.working_dir_invalid"
        ));
    }

    #[tokio::test]
    async fn treats_empty_working_directory_as_default_temp_dir() {
        let result = run_formatter(FormatterRunOptions {
            command: print_working_dir_command(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(8 * 1024),
            working_dir: Some("   ".into()),
            allow_shell: Some(true),
        })
        .await
        .expect("empty working directory should use default temp dir");

        let actual = PathBuf::from(result.stdout.trim())
            .canonicalize()
            .expect("reported working directory should resolve");
        let expected = std::env::temp_dir()
            .canonicalize()
            .expect("temp dir should resolve");
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    #[cfg_attr(windows, serial)]
    async fn returns_timed_out_result_after_killing_process() {
        let started = std::time::Instant::now();
        let result = run_formatter(FormatterRunOptions {
            command: sleep_command(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(50),
            max_output_bytes: Some(1024),
            working_dir: None,
            allow_shell: Some(true),
        })
        .await
        .expect("timeout should return recoverable result");
        let elapsed = started.elapsed();

        assert!(result.timed_out);
        assert_eq!(result.status_code, None);
        assert!(
            elapsed < Duration::from_secs(1),
            "timeout cleanup took {elapsed:?}"
        );
    }

    #[tokio::test]
    #[cfg_attr(windows, serial)]
    async fn returns_when_descendant_keeps_output_pipe_open() {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_formatter(FormatterRunOptions {
                command: exits_with_background_pipe_holder_command(),
                params: None,
                value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
                key: None,
                field: None,
                score: None,
                member: None,
                timeout_ms: Some(500),
                max_output_bytes: Some(1024),
                working_dir: None,
                allow_shell: Some(true),
            }),
        )
        .await
        .expect("formatter should not hang on inherited output pipes")
        .expect("formatter should return a recoverable result");
        let elapsed = started.elapsed();

        assert!(result.timed_out || result.status_code == Some(0));
        assert!(
            elapsed < Duration::from_secs(1),
            "formatter waited for descendant pipe holder: {elapsed:?}"
        );
    }

    #[tokio::test]
    #[cfg_attr(windows, serial)]
    async fn keeps_captured_output_when_reader_drain_times_out() {
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_formatter(FormatterRunOptions {
                command: marker_then_pipe_holder_command(),
                params: None,
                value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
                key: None,
                field: None,
                score: None,
                member: None,
                timeout_ms: Some(500),
                max_output_bytes: Some(1024),
                working_dir: None,
                allow_shell: Some(true),
            }),
        )
        .await
        .expect("formatter should not hang on inherited output pipes")
        .expect("formatter should return captured output");

        assert!(result.stdout.contains("formatter-marker"));
        assert!(result.timed_out);
        assert!(result.truncated);
    }

    #[tokio::test]
    async fn reader_drain_timeout_does_not_wait_for_non_cooperative_task() {
        let output = Arc::new(Mutex::new(CapturedOutput {
            bytes: b"early output".to_vec(),
            truncated: false,
        }));
        let task = tokio::task::spawn_blocking(|| {
            std::thread::sleep(Duration::from_secs(2));
            Ok(())
        });
        let started = std::time::Instant::now();

        let (output, aborted) =
            join_captured_output_with_timeout(task, output, Duration::from_millis(10))
                .await
                .expect("drain timeout should return fallback output");
        let elapsed = started.elapsed();

        assert!(aborted);
        assert_eq!(output.bytes, b"early output");
        assert!(output.truncated);
        assert!(
            elapsed < Duration::from_millis(500),
            "reader drain waited for natural task completion: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn truncates_output_when_limit_is_exceeded() {
        let result = run_formatter(FormatterRunOptions {
            command: rust_exe(),
            params: Some("--version".into()),
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(5),
            working_dir: None,
            allow_shell: None,
        })
        .await
        .expect("command should run with truncation");

        assert!(result.truncated);
        assert_eq!(result.stdout.len(), 5);
    }

    #[tokio::test]
    async fn truncates_combined_stdout_and_stderr_to_shared_limit() {
        let max_output_bytes = 5;
        let result = run_formatter(FormatterRunOptions {
            command: large_stdout_stderr_command(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: Some(5_000),
            max_output_bytes: Some(max_output_bytes),
            working_dir: None,
            allow_shell: Some(true),
        })
        .await
        .expect("command should run with combined output truncation");

        assert!(result.truncated);
        assert!(result.stdout.len() + result.stderr.len() <= max_output_bytes);
    }

    #[tokio::test]
    async fn rejects_shell_style_command_when_shell_is_disabled() {
        let err = run_formatter(FormatterRunOptions {
            command: "cargo --version".into(),
            params: None,
            value_base64: base64::engine::general_purpose::STANDARD.encode(b"unused"),
            key: None,
            field: None,
            score: None,
            member: None,
            timeout_ms: None,
            max_output_bytes: None,
            working_dir: None,
            allow_shell: Some(false),
        })
        .await
        .expect_err("space in executable should fail without shell");

        assert!(matches!(
            err,
            AppError::FormatterFailed { ref code, .. } if code == "formatter.command_missing"
        ));
    }
}
