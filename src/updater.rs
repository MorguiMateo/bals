use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
};
use windows::core::{HSTRING, PCWSTR};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO_OWNER: &str = "sconelli24";
pub const REPO_NAME: &str = "bals";

/// Información de un release disponible en GitHub Releases.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: String,
    pub download_url: String,
}

struct WinHttpHandle(*mut c_void);

impl WinHttpHandle {
    fn new(raw: *mut c_void, label: &str) -> Result<Self> {
        if raw.is_null() {
            bail!("{label} devolvió un handle nulo");
        }
        Ok(Self(raw))
    }
}

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// Elimina el binario `.old` que haya quedado de una actualización anterior.
pub fn cleanup_old_binary() {
    if let Ok(exe) = std::env::current_exe() {
        let old = exe.with_extension("exe.old");
        if old.exists() {
            match std::fs::remove_file(&old) {
                Ok(()) => debug!(path = %old.display(), "Binario anterior (.old) eliminado"),
                Err(err) => debug!("No se pudo eliminar {}: {err}", old.display()),
            }
        }
    }
}

/// Consulta `https://api.github.com/repos/sconelli24/bals/releases/latest` usando WinHTTP nativo
/// (sin OpenSSL ni crates HTTP externos) y devuelve `Some(ReleaseInfo)` si existe una versión mayor.
pub fn check_latest_release() -> Result<Option<ReleaseInfo>> {
    let api_path = format!("/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let body = https_get_bytes("api.github.com", &api_path, 512 * 1024)
        .context("No se pudo consultar el último release en GitHub")?;

    let json = String::from_utf8_lossy(&body);
    let Some(info) = parse_github_release_json(&json) else {
        return Ok(None);
    };

    if is_newer_version(&info.version, CURRENT_VERSION) {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

/// Descarga el nuevo `bals.exe`, verifica su cabecera PE (`MZ`), reemplaza el binario en ejecución
/// (renombrando el actual a `.old`), lanza la nueva versión y devuelve `Ok(())` para que el proceso actual cierre.
pub fn download_and_apply_update(info: &ReleaseInfo, launch_args: &[&str]) -> Result<()> {
    let current_exe = std::env::current_exe().context("No se pudo resolver current_exe")?;
    let new_exe = current_exe.with_extension("exe.new");
    let old_exe = current_exe.with_extension("exe.old");

    let (host, path) = split_https_url(&info.download_url)
        .context("URL de descarga inválida en el release de GitHub")?;

    info!(
        from = CURRENT_VERSION,
        to = %info.version,
        url = %info.download_url,
        "Descargando nueva versión de Bals..."
    );

    let bytes = https_get_bytes(&host, &path, 64 * 1024 * 1024)
        .context("Error descargando bals.exe desde GitHub Releases")?;

    // Verificación mínima de integridad: cabecera PE de Windows ("MZ") y tamaño > 512 KB.
    if bytes.len() < 512 * 1024 || &bytes[..2] != b"MZ" {
        bail!(
            "El archivo descargado ({} bytes) no parece ser un ejecutable Windows válido",
            bytes.len()
        );
    }

    write_file_atomic(&new_exe, &bytes)?;

    // Windows permite renombrar un .exe mientras está en ejecución.
    let _ = std::fs::remove_file(&old_exe);
    std::fs::rename(&current_exe, &old_exe)
        .context("No se pudo renombrar el bals.exe actual a .old")?;

    if let Err(err) = std::fs::rename(&new_exe, &current_exe) {
        // Rollback inmediato si falló el reemplazo.
        let _ = std::fs::rename(&old_exe, &current_exe);
        return Err(err).context("No se pudo mover la nueva versión a bals.exe");
    }

    info!(
        version = %info.version,
        path = %current_exe.display(),
        "Actualización aplicada; reiniciando Bals..."
    );

    if let Err(err) = Command::new(&current_exe).args(launch_args).spawn() {
        warn!("Actualizado correctamente, pero falló el reinicio automático: {err}");
    }

    Ok(())
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path)
        .with_context(|| format!("No se pudo crear archivo temporal {}", path.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

fn https_get_bytes(host: &str, path: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let agent = HSTRING::from(format!("Bals/{CURRENT_VERSION}"));
    let session = WinHttpHandle::new(
        unsafe {
            WinHttpOpen(
                &agent,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        },
        "WinHttpOpen",
    )?;

    let host_w = HSTRING::from(host);
    let connect = WinHttpHandle::new(
        unsafe { WinHttpConnect(session.0, &host_w, 443, 0) },
        "WinHttpConnect",
    )?;

    let verb = HSTRING::from("GET");
    let path_w = HSTRING::from(path);
    let request = WinHttpHandle::new(
        unsafe {
            WinHttpOpenRequest(
                connect.0,
                &verb,
                &path_w,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        },
        "WinHttpOpenRequest",
    )?;

    // Cabeceras estándar recomendadas por GitHub REST API.
    let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n"
        .encode_utf16()
        .collect();
    unsafe { WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0) }
        .context("WinHttpSendRequest falló")?;

    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
        .context("WinHttpReceiveResponse falló")?;

    let mut out = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 32 * 1024];

    loop {
        let mut bytes_read = 0u32;
        unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as u32,
                &mut bytes_read,
            )
        }
        .context("WinHttpReadData falló")?;

        if bytes_read == 0 {
            break;
        }
        if out.len() + bytes_read as usize > max_bytes {
            bail!("La descarga excedió el límite de seguridad ({max_bytes} bytes)");
        }
        out.extend_from_slice(&chunk[..bytes_read as usize]);
    }

    Ok(out)
}

fn split_https_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), path.to_string()))
}

/// Extrae `tag_name` y el `browser_download_url` de `bals.exe` de la respuesta JSON de GitHub Releases.
fn parse_github_release_json(json: &str) -> Option<ReleaseInfo> {
    let tag = extract_json_string_field(json, "tag_name")?;
    let version = tag.trim_start_matches(['v', 'V']).to_string();

    // Buscar un `browser_download_url` que termine en `/bals.exe` (ignorando zips o sha256).
    let mut search_from = 0usize;
    while let Some(pos) = json[search_from..].find("\"browser_download_url\"") {
        let abs = search_from + pos;
        if let Some(url) = extract_json_string_field(&json[abs..], "browser_download_url") {
            if url.to_ascii_lowercase().ends_with("/bals.exe") {
                return Some(ReleaseInfo {
                    version,
                    download_url: url,
                });
            }
        }
        search_from = abs + 22;
    }

    None
}

fn extract_json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let key_pos = text.find(&needle)?;
    let after_key = &text[key_pos + needle.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    let rest = after_colon.strip_prefix('"')?;

    let mut out = String::new();
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

/// Compara dos versiones semver `major.minor.patch`. Devuelve `true` si `remote > current`.
pub fn is_newer_version(remote: &str, current: &str) -> bool {
    match (parse_semver(remote), parse_semver(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let clean = s.trim().trim_start_matches(['v', 'V']);
    let clean = clean.split('-').next()?;
    let mut parts = clean.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semver_correctly() {
        assert!(is_newer_version("0.1.1", "0.1.0"));
        assert!(is_newer_version("v0.2.0", "0.1.9"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
    }

    #[test]
    fn parses_github_release_payload() {
        let sample = r#"{
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "bals-v0.2.0-windows-x64.zip", "browser_download_url": "https://github.com/sconelli24/bals/releases/download/v0.2.0/bals-v0.2.0-windows-x64.zip"},
                {"name": "bals.exe", "browser_download_url": "https://github.com/sconelli24/bals/releases/download/v0.2.0/bals.exe"}
            ]
        }"#;

        let info = parse_github_release_json(sample).expect("debería parsear release");
        assert_eq!(info.version, "0.2.0");
        assert_eq!(
            info.download_url,
            "https://github.com/sconelli24/bals/releases/download/v0.2.0/bals.exe"
        );
    }
}
