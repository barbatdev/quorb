//! Claude Pro / Max — límites de sesión (5 h) y semanales.
//!
//! No hay API pública para esto. Se reutiliza la sesión OAuth que ya guarda
//! Claude Code (`~/.claude/.credentials.json`) y se consulta el mismo endpoint
//! que usa `/usage`. Es NO OFICIAL: puede cambiar o romperse.
//!
//! Importante: la app NO refresca el token por su cuenta (rotaría el refresh
//! token y podría cerrar la sesión del CLI). Si caduca, se abre Claude Code,
//! que lo renueva solo.

use super::{home, net_error, read_json, resolve_path, Who};
use crate::config::SubscriptionConfig;
use crate::model::{Kind, Meter, Status, UsageReport};
use serde_json::Value;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Respaldo de macOS: Claude Code guarda la sesión OAuth en el Keychain
/// (servicio «Claude Code-credentials»), no en ~/.claude/.credentials.json —
/// ese archivo es el layout de Linux/WSL. La primera lectura desde la app
/// puede mostrar el diálogo de permiso de Keychain («Permitir siempre»). Devuelve
/// None si no hay entrada, el usuario niega el acceso o el secreto no parsea.
#[cfg(target_os = "macos")]
fn keychain_fallback() -> Option<Credentials> {
    let out = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_keychain_secret(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(not(target_os = "macos"))]
fn keychain_fallback() -> Option<Credentials> {
    None
}

/// Parser puro del secreto devuelto por `security … -w`: JSON con la misma
/// estructura que .credentials.json. Puro para poder probarlo sin Keychain.
#[cfg(any(test, target_os = "macos"))]
fn parse_keychain_secret(raw: &str) -> Option<Credentials> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(trimmed).ok().and_then(|v| Credentials::parse(&v).ok())
}

pub async fn fetch(cfg: &SubscriptionConfig, who: Who<'_>, http: &reqwest::Client) -> UsageReport {
    let report = who.report("Claude", Kind::Subscription);

    let candidates = vec![home().join(".claude").join(".credentials.json")];
    let creds = match resolve_path(cfg.credentials_path.as_deref(), &candidates) {
        Ok(p) => match read_json(&p).and_then(|v| Credentials::parse(&v)) {
            Ok(c) => c,
            Err(e) => return report.fail(Status::Error, e),
        },
        // El archivo no existe: en macOS Claude Code guarda la sesión OAuth en
        // el Keychain, no en ~/.claude. Se intenta el Keychain antes de fallar.
        Err(e) => match keychain_fallback() {
            Some(c) => c,
            None => {
                return report.fail(
                    Status::NeedsLogin,
                    format!("{e}. Inicia sesión en Claude Code."),
                )
            }
        },
    };

    let mut report = report;
    report.plan = creds.plan.clone();

    if creds.is_expired(chrono::Utc::now().timestamp_millis()) {
        return report.fail(
            Status::NeedsLogin,
            "La sesión ha caducado. «Renovar sesión» abre Claude Code, que la renueva; Quorb se actualiza solo.",
        );
    }

    let res = http
        .get(USAGE_URL)
        .bearer_auth(&creds.access_token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .header("anthropic-version", "2023-06-01")
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => return report.fail(Status::Error, net_error(e)),
    };
    match res.status().as_u16() {
        200 => {}
        401 | 403 => return report.fail(Status::NeedsLogin, "Token rechazado. «Renovar sesión» abre Claude Code para renovarlo; Quorb se actualiza solo."),
        429 => return report.fail(Status::Warning, "El endpoint de uso está limitando peticiones; reintento en el próximo ciclo."),
        code => return report.fail(Status::Error, format!("HTTP {code} en el endpoint de uso")),
    }
    let body: Value = match res.json().await {
        Ok(v) => v,
        Err(e) => return report.fail(Status::Error, format!("Respuesta inesperada: {e}")),
    };
    report.meters = parse_usage(&body);
    if report.meters.is_empty() {
        return report.fail(Status::Warning, "Respuesta sin datos de uso (¿ha cambiado el formato?)");
    }
    report
}

#[derive(Debug, PartialEq)]
pub(crate) struct Credentials {
    pub access_token: String,
    pub expires_at_ms: Option<i64>,
    pub plan: Option<String>,
}

impl Credentials {
    pub fn parse(v: &Value) -> Result<Self, String> {
        let o = v.get("claudeAiOauth").ok_or("Credenciales sin «claudeAiOauth» (¿login con API key?)")?;
        let access_token = o
            .get("accessToken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("Credenciales sin accessToken")?
            .to_string();
        Ok(Self {
            access_token,
            expires_at_ms: o.get("expiresAt").and_then(Value::as_i64),
            plan: o
                .get("subscriptionType")
                .and_then(Value::as_str)
                .map(pretty_plan),
        })
    }

    pub fn is_expired(&self, now_ms: i64) -> bool {
        // 60 s de margen
        self.expires_at_ms.map(|e| e - 60_000 <= now_ms).unwrap_or(false)
    }
}

fn pretty_plan(s: &str) -> String {
    match s.to_ascii_lowercase().as_str() {
        "max" => "Max".into(),
        "pro" => "Pro".into(),
        "team" => "Team".into(),
        "enterprise" => "Enterprise".into(),
        other => other.to_string(),
    }
}

pub(crate) fn parse_usage(v: &Value) -> Vec<Meter> {
    const H5: i64 = 5 * 3600;
    const D7: i64 = 7 * 86400;
    const WINDOWS: [(&str, &str, i64); 5] = [
        ("five_hour", "Sesión · 5 h", H5),
        ("seven_day", "Semana · todos", D7),
        ("seven_day_opus", "Semana · Opus", D7),
        ("seven_day_sonnet", "Semana · Sonnet", D7),
        ("seven_day_oauth_apps", "Semana · apps", D7),
    ];
    let mut out = vec![];
    for (key, label, secs) in WINDOWS {
        let Some(w) = v.get(key).filter(|w| w.is_object()) else { continue };
        let Some(pct) = w.get("utilization").and_then(Value::as_f64) else { continue };
        out.push(Meter {
            label: label.into(),
            percent: Some(pct),
            resets_at: w.get("resets_at").and_then(Value::as_str).map(str::to_string),
            window_seconds: Some(secs),
            ..Default::default()
        });
    }
    if let Some(extra) = v.get("extra_usage").filter(|e| e.get("is_enabled").and_then(Value::as_bool) == Some(true)) {
        out.push(Meter {
            label: "Uso extra (mes)".into(),
            percent: extra.get("utilization").and_then(Value::as_f64),
            used: extra.get("used_credits").and_then(Value::as_f64),
            limit: extra.get("monthly_limit").and_then(Value::as_f64),
            unit: Some("créditos".into()),
            resets_at: None,
            window_seconds: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_credentials() {
        let v = json!({"claudeAiOauth": {"accessToken": "sk-ant-oat01-x", "refreshToken": "r", "expiresAt": 1_900_000_000_000_i64, "subscriptionType": "max"}});
        let c = Credentials::parse(&v).unwrap();
        assert_eq!(c.plan.as_deref(), Some("Max"));
        assert!(!c.is_expired(1_800_000_000_000));
        assert!(c.is_expired(1_900_000_000_000));
    }

    #[test]
    fn rejects_missing_oauth() {
        assert!(Credentials::parse(&json!({})).is_err());
    }

    #[test]
    fn parses_usage_windows_and_skips_nulls() {
        let v = json!({
            "five_hour": {"utilization": 37.0, "resets_at": "2026-10-06T18:00:00+00:00"},
            "seven_day": {"utilization": 12.5, "resets_at": "2026-10-10T07:00:00+00:00"},
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 3.0, "resets_at": null},
            "extra_usage": {"is_enabled": false}
        });
        let m = parse_usage(&v);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].label, "Sesión · 5 h");
        assert_eq!(m[0].percent, Some(37.0));
        assert_eq!(m[2].resets_at, None);
        assert_eq!(m[0].window_seconds, Some(5 * 3600));
    }

    /// El secreto del Keychain (`security … -w`) usa la misma estructura JSON
    /// que .credentials.json; salida vacía o basura → None (sin panic).
    #[test]
    fn parses_keychain_secret_and_rejects_garbage() {
        let secret = r#"{"claudeAiOauth":{"accessToken":"tok","expiresAt":"2030-01-01T00:00:00Z","scopes":[]}}"#;
        let c = parse_keychain_secret(secret).expect("secreto válido");
        assert_eq!(c.access_token, "tok");
        assert!(parse_keychain_secret("").is_none());
        assert!(parse_keychain_secret("no es json").is_none());
        assert!(parse_keychain_secret("{\"otra\":1}").is_none());
    }
}
