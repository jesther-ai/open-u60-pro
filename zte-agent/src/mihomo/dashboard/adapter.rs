//! Small serving adapter for the SHA-pinned Zashboard 3.29.1 bundle. Keep
//! upstream files intact on disk and fail closed when their format changes.

use sha2::{Digest, Sha256};

const MAIN: &str = "assets/index-Cmoa7oGY.js";
const MAIN_SHA: &str = "882c26eb9783c137cf9875402aa94974e19abe2638f7f23249cda5ccf4da4682";
const INDEX_SHA: &str = "70bb3dc4057cbc0feaff524672f2b0fbafa52279b757dbdc8cb8fdc3e6a73510";
const BOOTSTRAP: &str = include_str!("bootstrap.js");
const MISMATCH: &str = "Dashboard version mismatch; turn off Mihomo and reinstall the pinned core & dashboard";

const REPLACEMENTS: &[(&str, &str)] = &[
    ("()=>Fd?.localStorage", "()=>localStorage"),
    (
        "ug=X(`setup/api-list`,[]);",
        "ug=X(`setup/api-list`,[]);ug.value=u60Dashboard.backends(ug.value);",
    ),
    (
        "var fg=Y(()=>ug.value.find(e=>e.uuid===dg.value))",
        "dg.value=u60Dashboard.uuid;var fg=Y(()=>ug.value.find(e=>e.uuid===dg.value))",
    ),
    (
        "wm=()=>{let e=new URLSearchParams(window.location.search||location.hash.match(/\\?.*$/)?.[0]?.replace(`?`,``));return e.has(`hostname`)?{type:e.get(`type`)===`dae`?`dae`:`clash`,protocol:Ree(e),secondaryPath:e.get(`secondaryPath`)||``,host:e.get(`hostname`),port:e.get(`port`),password:e.get(`secret`)||``,label:e.get(`label`)||``,disableUpgradeCore:e.get(`disableUpgradeCore`)===`1`||e.get(`disableUpgradeCore`)===`core`,disableTunMode:e.get(`disableTunMode`)===`1`||e.get(`disableTunMode`)===`tun`}:null}",
        "wm=()=>u60Dashboard.backend()",
    ),
    (
        "__name:`SetupPage`,setup(e){let t=I({type:`clash`,protocol:`http`,host:`127.0.0.1`,port:`9090`,secondaryPath:``,password:``,label:``})",
        "__name:`SetupPage`,setup(e){let t=I(u60Dashboard.backend())",
    ),
];

pub(super) fn asset(file: &str, data: Vec<u8>) -> Result<Vec<u8>, &'static str> {
    let sha = match file {
        "index.html" => INDEX_SHA,
        MAIN => MAIN_SHA,
        _ => return Ok(data),
    };
    if format!("{:x}", Sha256::digest(&data)) != sha {
        return Err(MISMATCH);
    }
    if file == MAIN {
        adapt(String::from_utf8(data).map_err(|_| MISMATCH)?).map(String::into_bytes)
    } else {
        Ok(data)
    }
}

fn adapt(mut js: String) -> Result<String, &'static str> {
    for (from, to) in REPLACEMENTS {
        let at = js.find(from).ok_or(MISMATCH)?;
        if js[at + from.len()..].contains(from) {
            return Err(MISMATCH);
        }
        js.replace_range(at..at + from.len(), to);
    }
    js.insert_str(0, BOOTSTRAP);
    Ok(js)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchors() -> String {
        REPLACEMENTS.iter().map(|(from, _)| *from).collect::<Vec<_>>().join(";\n")
    }

    #[test]
    fn adapter_initializes_backend_before_upstream_connection_and_prefills_setup() {
        let js = adapt(anchors()).unwrap();
        assert!(js.starts_with(BOOTSTRAP));
        for (_, to) in REPLACEMENTS {
            assert!(js.contains(to));
        }
        assert!(js.find("ug.value=u60Dashboard.backends").unwrap() < js.find("var fg=Y(").unwrap());
        assert!(js.find("dg.value=u60Dashboard.uuid").unwrap() < js.find("var fg=Y(").unwrap());
        assert!(!js.contains("host:`127.0.0.1`"));
        assert!(!js.contains("e.get(`secret`)"));
        assert!(BOOTSTRAP.contains(crate::mihomo::API));
    }

    #[test]
    fn unexpected_dashboard_versions_and_non_unique_anchors_fail_closed() {
        for source in [String::new(), anchors().replacen(REPLACEMENTS[0].0, "drift", 1), format!("{};{}", anchors(), REPLACEMENTS[0].0)] {
            assert!(adapt(source).is_err());
        }
        assert!(asset(MAIN, anchors().into_bytes()).is_err());
        assert!(asset("index.html", b"<html></html>".to_vec()).is_err());
        assert_eq!(asset("assets/style.css", b"unchanged".to_vec()).unwrap(), b"unchanged");
    }

    // Optional local QA: verify the real downloaded release and export the
    // exact adapted bytes for browser tests, without starting device services.
    #[test]
    fn pinned_distribution_browser_fixture() {
        let Ok(source) = std::env::var("U60_ZASHBOARD_DIST") else { return };
        for file in ["index.html", MAIN] {
            let data = asset(file, std::fs::read(std::path::Path::new(&source).join(file)).unwrap()).unwrap();
            if let Ok(out) = std::env::var("U60_WEB_QA_DIR") {
                let path = std::path::Path::new(&out).join("dashboard").join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, data).unwrap();
            }
        }
        if let Ok(out) = std::env::var("U60_WEB_QA_DIR") {
            let mut response = axum::response::Response::new(axum::body::Body::empty());
            crate::mihomo::dashboard::secure_dashboard(&mut response);
            let info = serde_json::json!({
                "location": crate::mihomo::dashboard::dashboard_link(),
                "csp": response.headers()[axum::http::header::CONTENT_SECURITY_POLICY].to_str().unwrap(),
            });
            std::fs::write(std::path::Path::new(&out).join("dashboard-entry.json"), serde_json::to_vec(&info).unwrap()).unwrap();
        }
    }
}
