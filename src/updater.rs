use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_DISABLE_REDIRECTS, WINHTTP_FLAG_REFRESH,
    WINHTTP_FLAG_SECURE, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2,
    WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3, WINHTTP_OPTION_DISABLE_FEATURE,
    WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
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

/// Consulta el último release en GitHub usando WinHTTP nativo con **TLS 1.2 explícito**
/// (imprescindible en Windows 10) y estrategia dual:
/// 1. Redirección `https://github.com/sconelli24/bals/releases/latest` (sin rate-limit de 60 req/h).
/// 2. Fallback a `https://api.github.com/repos/sconelli24/bals/releases/latest`.
pub fn check_latest_release() -> Result<Option<ReleaseInfo>> {
    let info = match check_via_github_redirect() {
        Ok(Some(info)) => info,
        Ok(None) => check_via_github_api()?,
        Err(redir_err) => {
            debug!("Comprobación vía redirect falló ({redir_err:#}); probando GitHub API");
            check_via_github_api()?
        }
    };

    if is_newer_version(&info.version, CURRENT_VERSION) {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

fn check_via_github_redirect() -> Result<Option<ReleaseInfo>> {
    let path = format!("/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let (status, location) = https_head_redirect("github.com", &path)?;
    if matches!(status, 301 | 302 | 303 | 307 | 308) {
        if let Some(loc) = location {
            if let Some(info) = parse_release_redirect_location(&loc) {
                return Ok(Some(info));
            }
        }
    }
    Ok(None)
}

fn check_via_github_api() -> Result<ReleaseInfo> {
    let api_path = format!("/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let body = https_get_bytes("api.github.com", &api_path, 512 * 1024)
        .context("No se pudo consultar el último release en GitHub API")?;

    let json = String::from_utf8_lossy(&body);
    parse_github_release_json(&json)
        .context("La respuesta de GitHub Releases no contenía un asset bals.exe válido")
}

/// Descarga y valida la cabecera PE (`MZ`) del nuevo `bals.exe` en memoria, sin cerrar aún el daemon.
pub fn download_update_bytes(info: &ReleaseInfo) -> Result<Vec<u8>> {
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

    Ok(bytes)
}

/// Reemplaza el binario en ejecución (renombrando el actual a `.old`) con `bytes` ya descargados
/// y relanza la nueva versión.
pub fn apply_downloaded_update(
    bytes: &[u8],
    info: &ReleaseInfo,
    launch_args: &[&str],
) -> Result<()> {
    let current_exe = std::env::current_exe().context("No se pudo resolver current_exe")?;
    let new_exe = current_exe.with_extension("exe.new");
    let old_exe = current_exe.with_extension("exe.old");

    write_file_atomic(&new_exe, bytes)?;

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

/// Descarga el nuevo `bals.exe`, verifica su cabecera PE (`MZ`), reemplaza el binario en ejecución
/// y relanza la nueva versión.
pub fn download_and_apply_update(info: &ReleaseInfo, launch_args: &[&str]) -> Result<()> {
    let bytes = download_update_bytes(info)?;
    apply_downloaded_update(&bytes, info, launch_args)
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create(path)
        .with_context(|| format!("No se pudo crear archivo temporal {}", path.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Abre una sesión WinHTTP configurando explícitamente TLS 1.2 (y TLS 1.3 si el SO lo soporta)
/// más timeouts razonables para evitar bloqueos y fallos TLS en Windows 10.
fn open_winhttp_session(proxy_access_type: WINHTTP_ACCESS_TYPE) -> Result<WinHttpHandle> {
    let agent = HSTRING::from(format!("Bals/{CURRENT_VERSION}"));
    let session = WinHttpHandle::new(
        unsafe {
            WinHttpOpen(
                &agent,
                proxy_access_type,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        },
        "WinHttpOpen",
    )?;

    // Forzar TLS 1.2+ (en Windows 10 WinHTTP a menudo deja solo TLS 1.0/1.1 por defecto y GitHub lo rechaza).
    // Si Windows 10 rechaza la bandera TLS 1.3 con ERROR_INVALID_PARAMETER, reintentar solo con TLS 1.2.
    let tls12_13: u32 =
        WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
    let ok_12_13 = unsafe {
        WinHttpSetOption(
            Some(session.0),
            WINHTTP_OPTION_SECURE_PROTOCOLS,
            Some(&tls12_13.to_ne_bytes()),
        )
    }
    .is_ok();

    if !ok_12_13 {
        let tls12: u32 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2;
        let _ = unsafe {
            WinHttpSetOption(
                Some(session.0),
                WINHTTP_OPTION_SECURE_PROTOCOLS,
                Some(&tls12.to_ne_bytes()),
            )
        };
    }

    let _ = unsafe { WinHttpSetTimeouts(session.0, 8_000, 8_000, 15_000, 30_000) };

    Ok(session)
}

fn query_http_status_code(request: *mut c_void) -> Result<u32> {
    let mut status = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast::<c_void>()),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .context("WinHttpQueryHeaders (STATUS_CODE) falló")?;
    Ok(status)
}

fn query_location_header(request: *mut c_void) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut byte_len = (buf.len() * 2) as u32;
    unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buf.as_mut_ptr().cast::<c_void>()),
            &mut byte_len,
            std::ptr::null_mut(),
        )
    }
    .ok()?;
    let u16_len = (byte_len as usize / 2).min(buf.len());
    let nul = buf[..u16_len].iter().position(|&c| c == 0).unwrap_or(u16_len);
    Some(String::from_utf16_lossy(&buf[..nul]))
}

/// Consulta `HEAD` sin seguir redirecciones para leer la cabecera `Location` en ~80 ms.
fn https_head_redirect(host: &str, path: &str) -> Result<(u32, Option<String>)> {
    let proxy_modes = [
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
        WINHTTP_ACCESS_TYPE_NO_PROXY,
    ];
    let mut last_err = None;
    for proxy_mode in proxy_modes {
        match https_head_redirect_once(host, path, proxy_mode) {
            Ok(res) => return Ok(res),
            Err(err) => {
                debug!(
                    proxy_mode = proxy_mode.0,
                    "HEAD WinHTTP falló ({err:#}); probando siguiente modo proxy"
                );
                last_err = Some(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Error consultando redirección HTTPS")))
}

fn https_head_redirect_once(
    host: &str,
    path: &str,
    proxy_mode: WINHTTP_ACCESS_TYPE,
) -> Result<(u32, Option<String>)> {
    let session = open_winhttp_session(proxy_mode)?;
    let host_w = HSTRING::from(host);
    let connect = WinHttpHandle::new(
        unsafe { WinHttpConnect(session.0, &host_w, 443, 0) },
        "WinHttpConnect",
    )?;

    let verb = HSTRING::from("HEAD");
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
                WINHTTP_FLAG_SECURE | WINHTTP_FLAG_REFRESH,
            )
        },
        "WinHttpOpenRequest",
    )?;

    let disable_redir: u32 = WINHTTP_DISABLE_REDIRECTS;
    let _ = unsafe {
        WinHttpSetOption(
            Some(request.0),
            WINHTTP_OPTION_DISABLE_FEATURE,
            Some(&disable_redir.to_ne_bytes()),
        )
    };

    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
        .context("WinHttpSendRequest (HEAD) falló")?;
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
        .context("WinHttpReceiveResponse (HEAD) falló")?;

    let status = query_http_status_code(request.0)?;
    let location = query_location_header(request.0);
    Ok((status, location))
}

fn https_get_bytes(host: &str, path: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let proxy_modes = [
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
        WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
        WINHTTP_ACCESS_TYPE_NO_PROXY,
    ];
    let mut last_err = None;
    for proxy_mode in proxy_modes {
        match https_get_bytes_once(host, path, max_bytes, proxy_mode) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => {
                debug!(
                    proxy_mode = proxy_mode.0,
                    "GET WinHTTP falló ({err:#}); probando siguiente modo proxy"
                );
                last_err = Some(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Error realizando petición HTTPS")))
}

fn https_get_bytes_once(
    host: &str,
    path: &str,
    max_bytes: usize,
    proxy_mode: WINHTTP_ACCESS_TYPE,
) -> Result<Vec<u8>> {
    let session = open_winhttp_session(proxy_mode)?;

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
                WINHTTP_FLAG_SECURE | WINHTTP_FLAG_REFRESH,
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

    let status = query_http_status_code(request.0)?;
    if status != 200 {
        bail!("GitHub respondió HTTP {status} para https://{host}{path}");
    }

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

/// Parsea una cabecera `Location` como `https://github.com/sconelli24/bals/releases/tag/v0.5.3`
/// y deriva la URL directa de descarga de `bals.exe`.
fn parse_release_redirect_location(location: &str) -> Option<ReleaseInfo> {
    let idx = location.find("/releases/tag/")?;
    let raw_tag = location[idx + "/releases/tag/".len()..]
        .split(['?', '#', '/'])
        .next()?
        .trim();
    if raw_tag.is_empty() {
        return None;
    }
    let version = raw_tag.trim_start_matches(['v', 'V']).to_string();
    parse_semver(&version)?;
    let download_url = format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{raw_tag}/bals.exe"
    );
    Some(ReleaseInfo {
        version,
        download_url,
    })
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

    #[test]
    fn parses_github_redirect_location() {
        let info = parse_release_redirect_location(
            "https://github.com/sconelli24/bals/releases/tag/v0.5.3",
        )
        .expect("debería parsear redirect");
        assert_eq!(info.version, "0.5.3");
        assert_eq!(
            info.download_url,
            "https://github.com/sconelli24/bals/releases/download/v0.5.3/bals.exe"
        );
        assert!(parse_release_redirect_location("https://github.com/sconelli24/bals/releases").is_none());
    }
}
