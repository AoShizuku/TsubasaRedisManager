import { call } from './invoke'
import type {
  CodecDecodeOptions,
  CodecDecodeResult,
  CodecDetectOptions,
  CodecDetectResult,
  CodecEncodeOptions,
  CodecEncodeResult,
  FormatterRunOptions,
  FormatterRunResult,
  ProtoFile,
  UpdateCheckOptions,
  UpdateCheckResult,
  WindowState,
} from '@/types/ipc'

/* ── 窗口 ─────────────────────────────────────────────── */
/* decorations 已关闭，标题栏是自绘的，所以这三个命令是唯一的窗口控制入口。 */

export function minimizeWindow(): Promise<void> {
  return call('native_minimize_window')
}

export function toggleMaximize(): Promise<void> {
  return call('native_toggle_maximize')
}

export function hideWindow(): Promise<void> {
  return call('native_hide_window')
}

export function getWindowState(): Promise<WindowState> {
  return call('native_get_window_state')
}

export function setZoomFactor(value: number): Promise<number> {
  return call('native_set_zoom_factor', { value })
}

/* ── 系统 ─────────────────────────────────────────────── */

/** 后端只放行 http/https，其余协议直接报错。 */
export function openExternal(url: string): Promise<void> {
  return call('native_open_external', { url })
}

/**
 * 用系统默认程序打开随包分发的第三方许可证全文。
 * 没有路径参数是有意的：路径由后端解析，前端给不了任意本地文件。
 */
export function openLicenses(): Promise<void> {
  return call('native_open_licenses')
}

export function getTempPath(): Promise<string> {
  return call('native_get_temp_path')
}

export function getFonts(): Promise<string[]> {
  return call('native_get_fonts')
}

export function checkUpdate(options: UpdateCheckOptions): Promise<UpdateCheckResult> {
  return call('native_check_update', { options })
}

export function readProtoFiles(paths: string[]): Promise<ProtoFile[]> {
  return call('native_read_proto_files', { paths })
}

/** Windows 上 .bat / .cmd 会被后端拒绝，这是刻意的，不要在前端绕过。 */
export function runFormatter(options: FormatterRunOptions): Promise<FormatterRunResult> {
  return call('native_run_formatter', { options })
}

/* ── 编解码 ───────────────────────────────────────────── */

export function codecDecode(options: CodecDecodeOptions): Promise<CodecDecodeResult> {
  return call('native_codec_decode', { options })
}

export function codecEncode(options: CodecEncodeOptions): Promise<CodecEncodeResult> {
  return call('native_codec_encode', { options })
}

export function codecDetect(options: CodecDetectOptions): Promise<CodecDetectResult> {
  return call('native_codec_detect', { options })
}
