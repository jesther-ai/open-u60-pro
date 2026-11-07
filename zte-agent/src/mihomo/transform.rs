//! Config preprocessing, not Mihomo's legacy `script:` feature. A fresh JS
//! child has no host bindings, and both engine and process resource limits.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_yaml_ng::{Mapping, Value};

use super::MAX_CONFIG;

pub const MAX_EXTENSION: usize = 128 * 1024;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extensions {
    pub merge_enabled: bool,
    pub script_enabled: bool,
    pub merge: String,
    pub script: String,
}

pub fn mapping(text: &str) -> Result<Value, String> {
    if text.len() > MAX_CONFIG {
        return Err("Configuration exceeds 512 KiB".into());
    }
    let mut value: Value =
        serde_yaml_ng::from_str(text).map_err(|_| "Invalid configuration YAML")?;
    value.apply_merge().map_err(|_| "Invalid YAML merge keys")?;
    if !value.is_mapping() {
        return Err(
            "Configuration must be a YAML mapping, not a node list or Base64 subscription".into(),
        );
    }
    Ok(value)
}

impl Extensions {
    pub fn validate(&self) -> Result<(), String> {
        if self.merge.len() > MAX_EXTENSION || self.script.len() > MAX_EXTENSION {
            return Err("Each extension is limited to 128 KiB".into());
        }
        if self.merge_enabled && !self.merge.trim().is_empty() {
            mapping(&self.merge)?;
        }
        Ok(())
    }
}

pub fn compile(source: &str, name: &str, extensions: &Extensions) -> Result<String, String> {
    extensions.validate()?;
    let mut config = mapping(source)?;
    if extensions.merge_enabled && !extensions.merge.trim().is_empty() {
        overlay(&mut config, mapping(&extensions.merge)?)?;
    }
    let mut yaml =
        serde_yaml_ng::to_string(&config).map_err(|_| "Cannot encode merged configuration")?;
    if extensions.script_enabled && !extensions.script.trim().is_empty() {
        yaml = script_process(&ScriptInput {
            config: yaml,
            name: name.into(),
            script: extensions.script.clone(),
        })?;
    }
    mapping(&yaml)?;
    Ok(yaml)
}

// Verge-style arrays replace. DNS is shallow at its first level; hosts replace.
// Explicit prepend-/append- keys are a U60 convenience, applied after replacement.
fn overlay(config: &mut Value, patch: Value) -> Result<(), String> {
    fn nested(target: &mut Value, patch: Value) {
        if let (Some(target), Some(patch)) = (target.as_mapping_mut(), patch.as_mapping()) {
            for (key, value) in patch {
                nested(
                    target.entry(key.clone()).or_insert(Value::Null),
                    value.clone(),
                );
            }
        } else {
            *target = patch;
        }
    }
    let root = config.as_mapping_mut().unwrap();
    let patch = patch.as_mapping().unwrap();
    for (key, value) in patch {
        let key = key
            .as_str()
            .ok_or("Merge keys must be strings")?
            .to_ascii_lowercase();
        if key.starts_with("prepend-") || key.starts_with("append-") {
            continue;
        }
        if key == "hosts" {
            root.insert(key.into(), value.clone());
        } else if key == "dns" && value.is_mapping() {
            let target = root
                .entry(key.into())
                .or_insert(Value::Mapping(Mapping::new()));
            if let Some(target) = target.as_mapping_mut() {
                target.extend(value.as_mapping().unwrap().clone());
            } else {
                *target = value.clone();
            }
        } else {
            nested(root.entry(key.into()).or_insert(Value::Null), value.clone());
        }
    }
    for field in ["rules", "proxies", "proxy-groups"] {
        for prepend in [true, false] {
            let key = format!("{}-{field}", if prepend { "prepend" } else { "append" });
            if let Some(extra) = patch.get(Value::from(key)) {
                let extra = extra
                    .as_sequence()
                    .ok_or("prepend-/append- values must be lists")?;
                let target = root
                    .entry(Value::from(field))
                    .or_insert(Value::Sequence(Vec::new()));
                let target = target
                    .as_sequence_mut()
                    .ok_or("Cannot extend a non-list field")?;
                if prepend {
                    target.splice(0..0, extra.clone());
                } else {
                    target.extend(extra.clone());
                }
            }
        }
    }
    for key in patch.keys().filter_map(Value::as_str) {
        if (key.starts_with("prepend-") || key.starts_with("append-"))
            && !["rules", "proxies", "proxy-groups"]
                .iter()
                .any(|field| key == format!("prepend-{field}") || key == format!("append-{field}"))
        {
            return Err("Only rules, proxies and proxy-groups support prepend-/append-".into());
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct ScriptInput {
    config: String,
    name: String,
    script: String,
}

const SCRIPT_FAILURE: &str = "Global script failed (syntax, exception, timeout, memory limit or invalid return). Return a synchronous config object from main(config, profileName); the previous configuration is unchanged";
const MAX_INPUT: u64 = (MAX_CONFIG + MAX_EXTENSION) as u64 * 6 + 4096;
const FRAME: &[u8] = b"U60SCRIPT\n";

fn evaluate(input: &ScriptInput) -> Result<String, String> {
    if input.script.len() > MAX_EXTENSION || input.name.len() > 128 {
        return Err(SCRIPT_FAILURE.into());
    }
    let config = mapping(&input.config)?;
    let json = serde_json::to_vec(&config).map_err(|_| SCRIPT_FAILURE)?;
    let runtime = rquickjs::Runtime::new().map_err(|_| SCRIPT_FAILURE)?;
    runtime.set_memory_limit(32 * 1024 * 1024);
    runtime.set_max_stack_size(512 * 1024);
    let deadline = Instant::now() + Duration::from_secs(2);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = rquickjs::Context::full(&runtime).map_err(|_| SCRIPT_FAILURE)?;
    let out = context.with(|ctx| -> rquickjs::Result<String> {
        ctx.globals().set("__config", ctx.json_parse(json)?)?;
        ctx.globals().set("__name", input.name.as_str())?;
        // Discard console output: configs and node credentials must not reach logs.
        ctx.eval::<(), _>("globalThis.console = Object.freeze({log(){},info(){},warn(){},error(){},debug(){},table(){}});")?;
        ctx.eval::<(), _>(input.script.as_bytes())?;
        ctx.eval::<String, _>(r#"
            (() => {
                const result = main(__config, __name);
                if (!result || typeof result !== 'object' || Array.isArray(result) || typeof result.then === 'function') throw Error();
                return JSON.stringify(result);
            })()
        "#)
    }).map_err(|_| SCRIPT_FAILURE)?;
    if out.len() > MAX_CONFIG {
        return Err("Script result exceeds 512 KiB".into());
    }
    // JSON is a YAML subset. Keep the same parser/limits for all entry paths.
    let config = mapping(&out)?;
    serde_yaml_ng::to_string(&config).map_err(|_| SCRIPT_FAILURE.into())
}

fn script_process(input: &ScriptInput) -> Result<String, String> {
    let mut command = Command::new(std::env::current_exe().map_err(|_| SCRIPT_FAILURE)?);
    #[cfg(not(test))]
    command.arg("mihomo-script-child");
    #[cfg(test)]
    command.args([
        "--ignored",
        "--exact",
        "mihomo::transform::tests::script_child_entry",
        "--nocapture",
    ]);
    let bytes = serde_json::to_vec(input).map_err(|_| SCRIPT_FAILURE)?;
    let mut child = command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| SCRIPT_FAILURE)?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    // Drain concurrently; a successful 512 KiB result cannot fit in a pipe.
    std::thread::scope(|scope| {
        let feed = scope.spawn(move || stdin.write_all(&bytes));
        let read = scope.spawn(move || {
            let mut output = Vec::new();
            stdout
                .take(MAX_CONFIG as u64 + 4096)
                .read_to_end(&mut output)
                .map(|_| output)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let success = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        let fed = feed.join().is_ok_and(|r| r.is_ok());
        let output = read
            .join()
            .map_err(|_| SCRIPT_FAILURE)?
            .map_err(|_| SCRIPT_FAILURE)?;
        if !success || !fed {
            return Err(SCRIPT_FAILURE.into());
        }
        let start = output
            .windows(FRAME.len())
            .position(|w| w == FRAME)
            .ok_or(SCRIPT_FAILURE)?
            + FRAME.len();
        let output = &output[start..];
        if output.len() > MAX_CONFIG {
            return Err(SCRIPT_FAILURE.into());
        }
        String::from_utf8(output.to_vec()).map_err(|_| SCRIPT_FAILURE.into())
    })
}

/// Internal entry point only: private JSON on stdin, processed YAML on stdout.
pub fn child() -> i32 {
    unsafe {
        let cap = libc::rlimit {
            rlim_cur: 3,
            rlim_max: 3,
        };
        libc::setrlimit(libc::RLIMIT_CPU, &cap);
        #[cfg(target_os = "linux")]
        {
            let cap = libc::rlimit {
                rlim_cur: 128 * 1024 * 1024,
                rlim_max: 128 * 1024 * 1024,
            };
            libc::setrlimit(libc::RLIMIT_AS, &cap);
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        }
    }
    let mut data = Vec::new();
    if std::io::stdin()
        .take(MAX_INPUT + 1)
        .read_to_end(&mut data)
        .is_err()
        || data.len() as u64 > MAX_INPUT
    {
        return 1;
    }
    let result = serde_json::from_slice::<ScriptInput>(&data)
        .map_err(|_| SCRIPT_FAILURE.to_owned())
        .and_then(|input| evaluate(&input));
    match result {
        Ok(yaml) if yaml.len() <= MAX_CONFIG => {
            let mut out = std::io::stdout().lock();
            i32::from(
                out.write_all(FRAME)
                    .and_then(|()| out.write_all(yaml.as_bytes()))
                    .is_err(),
            )
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "subprocess entry, never run directly"]
    fn script_child_entry() {
        // libtest writes a banner to stdout before this entry. Production has no banner.
        std::process::exit(child());
    }

    #[test]
    fn merge_has_explicit_array_and_dns_host_semantics() {
        let mut config = mapping("rules: [old]\ndns: {nameserver: [a], nameserver-policy: {old: a}}\nhosts: {old: 127.0.0.1}\n").unwrap();
        overlay(&mut config, mapping("rules: [new]\nprepend-rules: [first]\nappend-rules: [last]\ndns: {nameserver-policy: {new: b}}\nhosts: {}\n").unwrap()).unwrap();
        assert_eq!(
            config["rules"],
            serde_yaml_ng::to_value(["first", "new", "last"]).unwrap()
        );
        assert_eq!(config["dns"]["nameserver"][0], "a");
        assert!(config["dns"]["nameserver-policy"]["old"].is_null());
        assert!(config["hosts"].as_mapping().unwrap().is_empty());
    }

    #[test]
    fn scripts_support_modern_verge_syntax_and_bind_names_as_data() {
        let input = ScriptInput { config: "rules: [MATCH,DIRECT]".into(), name: "');throw Error();//".into(), script: r#"function main(c, name) { c.hosts = Object.fromEntries([...new Set(['test'])].map(x=>[x, name])); c.rules = ['first', ...(c.rules||[])]; return c; }"#.into() };
        let out = mapping(&evaluate(&input).unwrap()).unwrap();
        assert_eq!(out["hosts"]["test"], input.name);
        assert_eq!(out["rules"][0], "first");
    }

    #[test]
    fn scripts_cannot_access_host_io_or_return_promises_or_non_objects() {
        for script in [
            "function main(){ return []; }",
            "async function main(c){return c}",
            "function main(){return require('fs')}",
            "function main(){return fetch('https://example.com')}",
            "function main(){throw 'private-token'}",
        ] {
            let result = evaluate(&ScriptInput {
                config: "{}".into(),
                name: "test".into(),
                script: script.into(),
            })
            .unwrap_err();
            assert!(!result.contains("private-token"));
        }
    }

    #[test]
    fn runaway_script_is_interrupted() {
        let start = Instant::now();
        assert!(evaluate(&ScriptInput {
            config: "{}".into(),
            name: "test".into(),
            script: "function main(){while(true){}}".into()
        })
        .is_err());
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn isolated_pipeline_applies_merge_then_script_and_safeguards_last() {
        let extensions = Extensions { merge_enabled:true, script_enabled:true,
            merge:"prepend-rules: ['DOMAIN,first.test,DIRECT']".into(),
            script:"function main(c,n){c.rules.unshift('DOMAIN,'+n+',DIRECT'); c['external-controller']='0.0.0.0:1'; c.dns={ipv6:false}; return c;}".into() };
        let yaml = compile("rules: ['MATCH,DIRECT']", "second.test", &extensions).unwrap();
        let config = mapping(&super::super::managed_config(&yaml, "private").unwrap()).unwrap();
        assert_eq!(config["rules"][0], "DOMAIN,second.test,DIRECT");
        assert_eq!(config["rules"][1], "DOMAIN,first.test,DIRECT");
        assert_eq!(config["external-controller"], super::super::CONTROLLER);
        assert_eq!(config["dns"]["ipv6"], true);
    }

    #[test]
    fn isolated_failure_does_not_skip_script() {
        for script in [
            "function main(){while(true){}}",
            "function main(){ const a=[]; while(true) a.push('x'.repeat(100000)); }",
            "function main(){throw 'sensitive'}",
        ] {
            let extensions = Extensions {
                script_enabled: true,
                script: script.into(),
                ..Default::default()
            };
            let started = Instant::now();
            assert!(compile("{}", "QA", &extensions).is_err());
            assert!(started.elapsed() < Duration::from_secs(7));
        }
    }
}
