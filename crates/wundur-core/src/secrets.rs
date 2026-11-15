//! Secrets kept in the system's store, not in Wundur's settings file: the macOS Keychain (by
//! the `security` command), the Secret Service on Linux (by `secret-tool`). Windows is not
//! supported yet. Each secret is named by an account under Wundur's service.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::Mutex;

/// The service Wundur's secrets are kept under.
const SERVICE: &str = "wundur-studio";

/// The service they were kept under when the app was called Keleuo: a secret found only there
/// is copied to `SERVICE` the first time it is read (and left there, so Keleuo still has it).
const EARLIER: &str = "keleuo";

/// Whether this system can keep secrets, and if not, why.
pub fn supported() -> Result<(), String> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else if cfg!(target_os = "linux") {
        crate::host::find_program("secret-tool").map(|_| ()).ok_or_else(|| "Install secret-tool (libsecret) to keep keys.".into())
    } else {
        Err("Keeping keys is not supported on this system yet.".into())
    }
}

/// Which secrets are kept, as last looked up or changed here: asking the store is a process
/// each time (tens of milliseconds), too slow to do while drawing.
static KEPT: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

fn note(account: &str, kept: bool) {
    if let Ok(mut k) = KEPT.lock() {
        k.get_or_insert_with(HashMap::new).insert(account.to_string(), kept);
    }
}

/// Whether the secret named `account` is kept, as known without asking the store (`None`: not
/// looked up yet; [`kept`] looks).
pub fn known(account: &str) -> Option<bool> {
    KEPT.lock().ok()?.as_ref()?.get(account).copied()
}

/// Whether the secret named `account` is kept: from what is known, else the store (once).
pub fn kept(account: &str) -> bool {
    known(account).unwrap_or_else(|| get(account).is_some())
}

/// The secret named `account`, if there is one.
pub fn get(account: &str) -> Option<String> {
    let value = read(account);
    note(account, value.is_some());
    value
}

fn read(account: &str) -> Option<String> {
    if let Some(value) = read_in(SERVICE, account) {
        return Some(value);
    }
    // Kept under the app's earlier name: copied over. Removing it removes both, so the old one
    // doesn't come back.
    let value = read_in(EARLIER, account)?;
    let _ = set(account, &value);
    Some(value)
}

fn read_in(service: &str, account: &str) -> Option<String> {
    let out = if cfg!(target_os = "macos") {
        crate::platform::command("security").args(["find-generic-password", "-s", service, "-a", account, "-w"]).stderr(Stdio::null()).output().ok()?
    } else if cfg!(target_os = "linux") {
        crate::platform::command("secret-tool").args(["lookup", "service", service, "account", account]).stderr(Stdio::null()).output().ok()?
    } else {
        return None;
    };
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string();
    (!value.is_empty()).then_some(value)
}

/// `text` as one argument of a `security -i` command: in double quotes, with `"` and `\\`
/// escaped (a newline would end the command, so it cannot be kept).
fn quoted(text: &str) -> String {
    let escaped: String = text.chars().filter(|c| *c != '\n' && *c != '\r').flat_map(|c| if c == '"' || c == '\\' { vec!['\\', c] } else { vec![c] }).collect();
    format!("\"{escaped}\"")
}

/// Keep `value` as the secret named `account` (replacing one there).
pub fn set(account: &str, value: &str) -> io::Result<()> {
    supported().map_err(io::Error::other)?;
    let status = if cfg!(target_os = "macos") {
        // `security -i` reads its command on standard input, so the value is never on a command
        // line (where any process listing would show it). -U: update it if it is there.
        let line = format!("add-generic-password -U -s {} -a {} -w {}\n", quoted(SERVICE), quoted(account), quoted(value));
        let mut child = crate::platform::command("security")
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        use std::io::Write;
        child.stdin.take().map(|mut i| i.write_all(line.as_bytes())).transpose()?;
        let out = child.wait_with_output()?;
        // In this mode it exits well whatever happened: a failed command says so on stderr.
        if !out.stderr.is_empty() {
            return Err(io::Error::other(format!("the Keychain refused the key: {}", String::from_utf8_lossy(&out.stderr).trim())));
        }
        out.status
    } else {
        // secret-tool reads the value on its standard input.
        let mut child = crate::platform::command("secret-tool")
            .args(["store", "--label", &format!("Wundur: {account}"), "service", SERVICE, "account", account])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        use std::io::Write;
        child.stdin.take().map(|mut i| i.write_all(value.as_bytes())).transpose()?;
        child.wait()?
    };
    if !status.success() {
        return Err(io::Error::other("the system's key store refused it"));
    }
    note(account, true);
    Ok(())
}

/// Forget the secret named `account`.
pub fn remove(account: &str) {
    note(account, false);
    remove_in(SERVICE, account);
    remove_in(EARLIER, account);
}

fn remove_in(service: &str, account: &str) {
    if cfg!(target_os = "macos") {
        let _ = crate::platform::command("security").args(["delete-generic-password", "-s", service, "-a", account]).output();
    } else if cfg!(target_os = "linux") {
        let _ = crate::platform::command("secret-tool").args(["clear", "service", service, "account", account]).output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uses this Mac's Keychain (a throwaway entry), so not run by default:
    /// `cargo test -p wundur-core secrets -- --ignored`.
    #[test]
    fn a_value_is_quoted_for_security_s_own_parser() {
        assert_eq!(quoted("kel_abc"), "\"kel_abc\"");
        assert_eq!(quoted("a\"b\\c d"), "\"a\\\"b\\\\c d\"");
        assert_eq!(quoted("one\ntwo"), "\"onetwo\"");
    }

    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn a_key_kept_under_the_earlier_name_is_copied_over() {
        let account = format!("rename-test-{}", std::process::id());
        let status = std::process::Command::new("security").args(["add-generic-password", "-U", "-s", EARLIER, "-a", &account, "-w", "not-a-real-key"]).status().unwrap();
        assert!(status.success());
        assert_eq!(get(&account).as_deref(), Some("not-a-real-key"), "found under the earlier name");
        assert_eq!(read_in(SERVICE, &account).as_deref(), Some("not-a-real-key"), "now under the new one");
        assert_eq!(read_in(EARLIER, &account).as_deref(), Some("not-a-real-key"), "and still under the old one");
        remove(&account);
        assert_eq!((read_in(SERVICE, &account), read_in(EARLIER, &account)), (None, None), "removed from both");
    }
}
