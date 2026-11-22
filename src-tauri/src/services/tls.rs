use crate::error::{AppError, AppResult};
use crate::models::SslOptions;
use redis::{ClientTlsConfig, TlsCertificates};
use std::fs;
use std::sync::Once;

static RUSTLS_CRYPTO_PROVIDER: Once = Once::new();

pub fn ensure_crypto_provider() {
    RUSTLS_CRYPTO_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn read_tls_certificates(options: &SslOptions) -> AppResult<Option<TlsCertificates>> {
    let key_path = non_blank_path(options.key.as_deref());
    let cert_path = non_blank_path(options.cert.as_deref());
    let ca_path = non_blank_path(options.ca.as_deref());

    if key_path.is_none() && cert_path.is_none() && ca_path.is_none() {
        return Ok(None);
    }

    if key_path.is_some() != cert_path.is_some() {
        return Err(AppError::RedisTlsFailed(
            "TLS client certificate and key must be configured together".into(),
        ));
    }

    let root_cert = ca_path
        .map(|path| read_tls_file("CA certificate", path))
        .transpose()?;
    let client_tls = match (cert_path, key_path) {
        (Some(cert_path), Some(key_path)) => Some(ClientTlsConfig {
            client_cert: read_tls_file("client certificate", cert_path)?,
            client_key: read_tls_file("client key", key_path)?,
        }),
        _ => None,
    };

    Ok(Some(TlsCertificates {
        client_tls,
        root_cert,
    }))
}

fn non_blank_path(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn read_tls_file(label: &str, path: &str) -> AppResult<Vec<u8>> {
    fs::read(path).map_err(|err| {
        AppError::RedisTlsFailed(format!("Failed to read TLS {label} file '{path}': {err}"))
    })
}

#[cfg(test)]
mod tests {
    use super::read_tls_certificates;
    use crate::models::SslOptions;
    use std::fs;

    #[test]
    fn returns_none_when_no_certificate_files_are_configured() {
        let options = SslOptions {
            key: None,
            cert: None,
            ca: None,
            servername: None,
            insecure: None,
        };
        assert!(read_tls_certificates(&options).unwrap().is_none());
    }

    #[test]
    fn treats_blank_certificate_paths_as_missing() {
        let options = SslOptions {
            key: Some(" ".into()),
            cert: Some("\t".into()),
            ca: Some("\n".into()),
            servername: Some("redis.local".into()),
            insecure: None,
        };
        assert!(read_tls_certificates(&options).unwrap().is_none());
    }

    #[test]
    fn reads_ca_and_client_pair() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("client.key");
        let cert = dir.path().join("client.crt");
        let ca = dir.path().join("ca.crt");
        fs::write(&key, "key").unwrap();
        fs::write(&cert, "cert").unwrap();
        fs::write(&ca, "ca").unwrap();

        let options = SslOptions {
            key: Some(key.to_string_lossy().to_string()),
            cert: Some(cert.to_string_lossy().to_string()),
            ca: Some(ca.to_string_lossy().to_string()),
            servername: Some("redis.local".into()),
            insecure: None,
        };

        let certs = read_tls_certificates(&options).unwrap().unwrap();
        assert_eq!(certs.root_cert.unwrap(), b"ca".to_vec());
        assert_eq!(
            certs.client_tls.as_ref().unwrap().client_cert,
            b"cert".to_vec()
        );
        assert_eq!(certs.client_tls.unwrap().client_key, b"key".to_vec());
    }

    #[test]
    fn requires_client_certificate_and_key_together() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("client.key");
        fs::write(&key, "key").unwrap();

        let options = SslOptions {
            key: Some(key.to_string_lossy().to_string()),
            cert: None,
            ca: None,
            servername: None,
            insecure: None,
        };

        let err = match read_tls_certificates(&options) {
            Ok(_) => panic!("expected missing client certificate error"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            crate::error::AppError::RedisTlsFailed(detail)
                if detail == "TLS client certificate and key must be configured together"
        ));
    }

    #[test]
    fn maps_file_read_errors_to_tls_errors_with_path_detail() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("missing-ca.crt");
        let options = SslOptions {
            key: None,
            cert: None,
            ca: Some(ca.to_string_lossy().to_string()),
            servername: None,
            insecure: None,
        };

        let err = match read_tls_certificates(&options) {
            Ok(_) => panic!("expected TLS file read error"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            crate::error::AppError::RedisTlsFailed(detail)
                if detail.contains("missing-ca.crt")
                    && detail.contains("Failed to read TLS CA certificate file")
        ));
    }
}
