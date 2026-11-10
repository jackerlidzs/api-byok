//! Installs and manages the local certificate authority.
use std::{fs, path::PathBuf};

#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, RsaKeySize, PKCS_RSA_SHA256,
};
#[cfg(target_os = "macos")]
use sha1::{Digest, Sha1};
use time::{Duration, OffsetDateTime};
use x509_parser::prelude::FromDer;

use crate::{config::managed_data_dir, Error, Result};

use super::CaState;

#[derive(Clone)]
pub struct CaManager {
    dir: PathBuf,
}

pub struct LoadedCa {
    pub issuer: Issuer<'static, KeyPair>,
}

impl CaManager {
    pub fn managed() -> Result<Self> {
        Ok(Self {
            dir: managed_data_dir()?.join("ca"),
        })
    }

    fn cert_path(&self) -> PathBuf {
        self.dir.join("ca.crt")
    }
    fn key_path(&self) -> PathBuf {
        self.dir.join("ca.key")
    }

    pub fn state(&self) -> Result<CaState> {
        let cert = fs::read_to_string(self.cert_path());
        let key = fs::read_to_string(self.key_path());
        match (cert, key) {
            (Err(cert_error), Err(key_error))
                if cert_error.kind() == std::io::ErrorKind::NotFound
                    && key_error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(CaState::Missing)
            }
            (Ok(cert), Ok(key)) => {
                if parse_issuer(&cert, &key).is_err() {
                    return Ok(CaState::Invalid);
                }
                Ok(if is_installed(&cert)? {
                    CaState::Ready
                } else {
                    CaState::Untrusted
                })
            }
            _ => Ok(CaState::Invalid),
        }
    }

    pub fn load(&self) -> Result<LoadedCa> {
        let cert = fs::read_to_string(self.cert_path())?;
        let key = fs::read_to_string(self.key_path())?;
        Ok(LoadedCa {
            issuer: parse_issuer(&cert, &key)?,
        })
    }

    pub fn install_command(&self) -> Option<String> {
        let path = self.cert_path().to_string_lossy().replace('\'', "'\\''");
        match std::env::consts::OS {
            "macos" => dirs::home_dir().map(|_| {
                format!(
                    "sudo security add-trusted-cert -d -r trustRoot -p ssl -k /Library/Keychains/System.keychain '{}'",
                    path
                )
            }),
            "windows" => Some(format!(
                "certutil -addstore -f Root \"{}\"",
                self.cert_path().display()
            )),
            "linux" => {
                let anchor = linux_anchor_file();
                // Containers (e.g. Render) run as root and usually have no sudo.
                let sudo = if running_as_root() { "" } else { "sudo " };
                Some(format!(
                    "{sudo}cp '{}' '{}' && {sudo}{}",
                    path,
                    anchor.display(),
                    linux_refresh_command()
                ))
            }
            _ => None,
        }
    }

    pub fn initialize_local(&self) -> Result<()> {
        match self.state()? {
            CaState::Invalid => {
                return Err(Error::Config("CA files are incomplete or invalid".into()))
            }
            CaState::Ready => return Ok(()),
            CaState::Missing => self.generate()?,
            CaState::Untrusted => {}
        }
        self.trust_automatically()?;
        Ok(())
    }

    /// With `CURSOR_TRUST_CA=1` on Linux as root (a container), adds the existing CA to the
    /// system trust store, so it is trusted again after each redeploy without a manual command.
    /// Returns whether the CA is trusted afterwards.
    pub fn trust_automatically(&self) -> Result<bool> {
        if !auto_trust_enabled() {
            return Ok(false);
        }
        let cert = match fs::read_to_string(self.cert_path()) {
            Ok(cert) => cert,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if is_installed(&cert)? {
            return Ok(true);
        }
        let anchor = linux_anchor_file();
        if let Some(dir) = anchor.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&anchor, &cert)?;
        let mut command = linux_refresh_command().split_whitespace();
        let program = command.next().unwrap_or("update-ca-certificates");
        let status = std::process::Command::new(program)
            .args(command)
            .stdout(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            return Err(Error::Config(format!(
                "{} failed with {status}",
                linux_refresh_command()
            )));
        }
        Ok(true)
    }

    fn generate(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;

        let key = KeyPair::generate_rsa_for(&PKCS_RSA_SHA256, RsaKeySize::_3072)
            .map_err(|error| Error::Config(format!("generate CA key: {error}")))?;
        let mut params = CertificateParams::new(Vec::<String>::new())
            .map_err(|error| Error::Config(format!("create CA parameters: {error}")))?;
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, "Cursor BYOK Local CA");
        name.push(DnType::OrganizationName, "Cursor BYOK");
        params.distinguished_name = name;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        params.not_before = OffsetDateTime::now_utc() - Duration::minutes(5);
        params.not_after = OffsetDateTime::now_utc() + Duration::days(3652);
        let cert = params
            .self_signed(&key)
            .map_err(|error| Error::Config(format!("generate CA certificate: {error}")))?;
        write_atomic(&self.key_path(), key.serialize_pem().as_bytes(), 0o600)?;
        write_atomic(&self.cert_path(), cert.pem().as_bytes(), 0o644)?;
        Ok(())
    }
}

fn parse_issuer(cert: &str, key: &str) -> Result<Issuer<'static, KeyPair>> {
    let key =
        KeyPair::from_pem(key).map_err(|error| Error::Config(format!("parse CA key: {error}")))?;
    let pem = pem::parse(cert).map_err(|error| Error::Config(format!("parse CA PEM: {error}")))?;
    let (_, parsed) = x509_parser::certificate::X509Certificate::from_der(pem.contents())
        .map_err(|error| Error::Config(format!("parse CA X.509 certificate: {error}")))?;
    if parsed.public_key().subject_public_key.data.as_ref() != key.public_key_raw() {
        return Err(Error::Config(
            "CA certificate and private key do not match".into(),
        ));
    }
    if !parsed.validity().is_valid() {
        return Err(Error::Config(
            "CA certificate is outside its validity period".into(),
        ));
    }
    if !parsed
        .basic_constraints()
        .map_err(|error| Error::Config(format!("read CA constraints: {error}")))?
        .is_some_and(|constraints| constraints.value.ca)
    {
        return Err(Error::Config("certificate is not a CA".into()));
    }
    Issuer::from_ca_cert_pem(cert, key)
        .map_err(|error| Error::Config(format!("parse CA certificate: {error}")))
}

fn write_atomic(path: &std::path::Path, data: &[u8], _mode: u32) -> Result<()> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, data)?;
    #[cfg(unix)]
    fs::set_permissions(&temp, fs::Permissions::from_mode(_mode))?;
    fs::rename(&temp, path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(_mode))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn fingerprint(cert: &str) -> Result<String> {
    let pem = pem::parse(cert).map_err(|error| Error::Config(format!("parse CA PEM: {error}")))?;
    Ok(hex::encode_upper(Sha1::digest(pem.contents())))
}

#[cfg(target_os = "macos")]
fn is_installed(cert: &str) -> Result<bool> {
    let fingerprint = fingerprint(cert)?;
    for keychain in ["login.keychain-db", "/Library/Keychains/System.keychain"] {
        let output = Command::new("security")
            .args(["find-certificate", "-a", "-Z", keychain])
            .output()?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(&fingerprint)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "windows")]
fn is_installed(cert: &str) -> Result<bool> {
    windows::is_installed(cert)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn is_installed(cert: &str) -> Result<bool> {
    match fs::read_to_string(linux_anchor_file()) {
        Ok(installed) => Ok(installed.trim() == cert.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Whether the effective user is root, read from `/proc/self/status` (Linux only).
fn running_as_root() -> bool {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|line| {
                    line.strip_prefix("Uid:")
                        .map(|ids| ids.split_whitespace().nth(1) == Some("0"))
                })
            })
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn auto_trust_enabled() -> bool {
    let requested = std::env::var("CURSOR_TRUST_CA")
        .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes"));
    requested && running_as_root()
}

const LINUX_ANCHOR_NAME: &str = "cursor-byok-local-ca.crt";

fn linux_anchor_file() -> PathBuf {
    if PathBuf::from("/etc/pki/ca-trust/source/anchors").is_dir() {
        PathBuf::from("/etc/pki/ca-trust/source/anchors").join(LINUX_ANCHOR_NAME)
    } else if PathBuf::from("/etc/ca-certificates/trust-source/anchors").is_dir() {
        PathBuf::from("/etc/ca-certificates/trust-source/anchors").join(LINUX_ANCHOR_NAME)
    } else {
        PathBuf::from("/usr/local/share/ca-certificates").join(LINUX_ANCHOR_NAME)
    }
}

fn linux_refresh_command() -> &'static str {
    match linux_anchor_file().parent().and_then(|dir| dir.to_str()) {
        Some("/usr/local/share/ca-certificates") => "update-ca-certificates",
        _ => "update-ca-trust extract",
    }
}
