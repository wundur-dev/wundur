//! Shell integration: a shell started in a Wundur terminal loads a small script (after the
//! person's own startup files) that marks where each prompt, command and output begins, and
//! how the command ended, with the `OSC 133` sequences other terminals use (plus `OSC 633;E`
//! for the command's text). See `docs/proposals/terminal-commands.md`.
//!
//! zsh, bash, fish and PowerShell are supported; any other shell starts as it always did.
//! Nothing is written to the person's home folder: the scripts live in Wundur's cache and are
//! loaded through the shell's own startup mechanism (`ZDOTDIR`, `--rcfile`, `XDG_DATA_DIRS`,
//! `-Command`).

use std::path::{Path, PathBuf};

use crate::host::CommandSpec;

/// A shell Wundur knows how to integrate with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
    PowerShell,
}

impl Shell {
    /// The shell a program is, by its file name.
    pub fn of(program: &Path) -> Option<Self> {
        // Either separator: a Windows path is named the same on any system.
        let name = program.to_str()?.rsplit(['/', '\\']).next()?.to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            "pwsh" | "powershell" => Some(Self::PowerShell),
            _ => None,
        }
    }
}

/// Where the scripts are written: Wundur's cache (`WUNDUR_CACHE_DIR` when set, for tests).
pub fn dir() -> Option<PathBuf> {
    crate::images::root().and_then(|images| images.parent().map(|p| p.join("shell-integration")))
}

/// `spec` changed to load the integration script, the scripts written under `dir`; `None`
/// when the program is not a shell Wundur knows (it then starts unchanged).
pub fn integrate(spec: &CommandSpec, dir: &Path) -> Option<CommandSpec> {
    let shell = Shell::of(&spec.program)?;
    let mut out = spec.clone();
    let write = |rel: &str, text: &str| -> Option<PathBuf> {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent()?).ok()?;
        // Written only when changed, so a running shell never reads half a file.
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            std::fs::write(&path, text).ok()?;
        }
        Some(path)
    };
    match shell {
        Shell::Zsh => {
            for (name, text) in [(".zshenv", ZSH_ENV), (".zprofile", ZSH_PROFILE), (".zshrc", ZSH_RC), (".zlogin", ZSH_LOGIN)] {
                write(&format!("zsh/{name}"), text)?;
            }
            // The person's own ZDOTDIR (or none: their home), for the scripts to load from.
            if let Some(own) = std::env::var_os("ZDOTDIR") {
                out.env.push(("WUNDUR_USER_ZDOTDIR".into(), own.to_string_lossy().into_owned()));
            }
            out.env.push(("ZDOTDIR".into(), dir.join("zsh").to_string_lossy().into_owned()));
        }
        Shell::Bash => {
            let rc = write("bash/wundur.bash", BASH_RC)?;
            // A login shell ignores `--rcfile`: the script loads the login files itself.
            let login = out.args.iter().any(|a| a == "-l" || a == "--login");
            out.args.retain(|a| a != "-l" && a != "--login");
            out.args.splice(0..0, ["--rcfile".to_string(), rc.to_string_lossy().into_owned(), "-i".to_string()]);
            if login {
                out.env.push(("WUNDUR_BASH_LOGIN".into(), "1".into()));
            }
        }
        Shell::Fish => {
            write("fish/fish/vendor_conf.d/wundur.fish", FISH_RC)?;
            let own = std::env::var("XDG_DATA_DIRS").ok().filter(|s| !s.is_empty());
            let sep = if cfg!(windows) { ";" } else { ":" };
            let rest = own.unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            out.env.push(("XDG_DATA_DIRS".into(), format!("{}{sep}{rest}", dir.join("fish").display())));
        }
        Shell::PowerShell => {
            let script = write("pwsh/wundur.ps1", PWSH_RC)?;
            let quoted = script.to_string_lossy().replace('\'', "''");
            out.args.extend(["-NoExit".to_string(), "-Command".to_string(), format!(". '{quoted}'")]);
        }
    }
    out.env.push(("WUNDUR_SHELL_INTEGRATION".into(), "1".into()));
    Some(out)
}

/// zsh reads `.zshenv`, `.zprofile` (login), `.zshrc` (interactive) and `.zlogin` (login) from
/// `ZDOTDIR`. Each of Wundur's loads the person's own from their ZDOTDIR (or home), then points
/// ZDOTDIR back at Wundur's folder for the next file; the last one leaves the person's.
const ZSH_ENV: &str = r#"# Wundur shell integration (zsh): loads your own .zshenv.
__wundur_zdotdir=$ZDOTDIR
ZDOTDIR=${WUNDUR_USER_ZDOTDIR:-$HOME}
[[ -f $ZDOTDIR/.zshenv ]] && source $ZDOTDIR/.zshenv
WUNDUR_USER_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__wundur_zdotdir
"#;

const ZSH_PROFILE: &str = r#"# Wundur shell integration (zsh): loads your own .zprofile.
ZDOTDIR=$WUNDUR_USER_ZDOTDIR
[[ -f $ZDOTDIR/.zprofile ]] && source $ZDOTDIR/.zprofile
WUNDUR_USER_ZDOTDIR=$ZDOTDIR
ZDOTDIR=$__wundur_zdotdir
"#;

const ZSH_RC: &str = r#"# Wundur shell integration (zsh): loads your own .zshrc, then marks prompts and commands.
ZDOTDIR=$WUNDUR_USER_ZDOTDIR
[[ -f $ZDOTDIR/.zshrc ]] && source $ZDOTDIR/.zshrc
WUNDUR_USER_ZDOTDIR=$ZDOTDIR
if [[ -o login ]]; then
  ZDOTDIR=$__wundur_zdotdir
else
  unset WUNDUR_USER_ZDOTDIR __wundur_zdotdir
fi

if [[ -z $__wundur_loaded ]]; then
  __wundur_loaded=1
  __wundur_esc() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//;/\\x3b}
    s=${s//$'\n'/\\x0a}
    s=${s//$'\e'/}
    s=${s//$'\a'/}
    print -rn -- "$s"
  }
  __wundur_precmd() {
    local ret=$?
    if [[ -n $__wundur_ran ]]; then
      print -rn -- $'\e]133;D;'"$ret"$'\a'
    fi
    __wundur_ran=
    # Themes set the prompt anew each time: mark it again if it lost its marks.
    if [[ $PS1 != *$'\e]133;B'* ]]; then
      PS1="%{"$'\e]133;A\a'"%}${PS1}%{"$'\e]133;B\a'"%}"
    fi
  }
  __wundur_preexec() {
    __wundur_ran=1
    print -rn -- $'\e]633;E;'"$(__wundur_esc "$1")"$'\a\e]133;C\a'
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd __wundur_precmd
  add-zsh-hook preexec __wundur_preexec
  print -rn -- $'\e]633;P;Shell=zsh\a'
fi
"#;

const ZSH_LOGIN: &str = r#"# Wundur shell integration (zsh): loads your own .zlogin.
ZDOTDIR=$WUNDUR_USER_ZDOTDIR
[[ -f $ZDOTDIR/.zlogin ]] && source $ZDOTDIR/.zlogin
unset WUNDUR_USER_ZDOTDIR __wundur_zdotdir
"#;

const BASH_RC: &str = r#"# Wundur shell integration (bash): loads your own startup files, then marks prompts and commands.
if [[ -n $WUNDUR_BASH_LOGIN ]]; then
  unset WUNDUR_BASH_LOGIN
  [[ -f /etc/profile ]] && source /etc/profile
  if [[ -f ~/.bash_profile ]]; then source ~/.bash_profile
  elif [[ -f ~/.bash_login ]]; then source ~/.bash_login
  elif [[ -f ~/.profile ]]; then source ~/.profile
  fi
else
  [[ -f ~/.bashrc ]] && source ~/.bashrc
fi

if [[ -z $__wundur_loaded ]]; then
  __wundur_loaded=1
  __wundur_esc() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//;/\\x3b}
    s=${s//$'\n'/\\x0a}
    printf '%s' "$s"
  }
  __wundur_prompt() {
    local ret=$?
    # Only counted after a command ran (the terminal ignores it otherwise).
    printf '\e]133;D;%s\a\e]133;A\a' "$ret"
    return $ret
  }
  __wundur_after() {
    local ret=$?
    if [[ $PS1 != *'133;B'* ]]; then
      PS1="${PS1}"'\[\e]133;B\a\]'
    fi
    __wundur_armed=1
    return $ret
  }
  __wundur_preexec() {
    local c
    c=$(HISTTIMEFORMAT= builtin history 1)
    c=${c#*[0-9]  }
    printf '\e]633;E;%s\a\e]133;C\a' "$(__wundur_esc "$c")"
  }
  if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
    PROMPT_COMMAND=(__wundur_prompt "${PROMPT_COMMAND[@]}" __wundur_after)
  else
    PROMPT_COMMAND="__wundur_prompt;${PROMPT_COMMAND:+$PROMPT_COMMAND;}__wundur_after"
  fi
  if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    # Printed as a command starts.
    PS0="${PS0}"'$(__wundur_preexec)'
  elif [[ -z $(trap -p DEBUG) ]]; then
    # Older bash (macOS's 3.2): the first command run after a prompt.
    __wundur_debug() {
      [[ -n $__wundur_armed && -z $COMP_LINE ]] || return
      case $BASH_COMMAND in __wundur_*) return ;; esac
      __wundur_armed=
      __wundur_preexec
    }
    trap '__wundur_debug' DEBUG
  fi
  printf '\e]633;P;Shell=bash\a'
fi
"#;

const FISH_RC: &str = r#"# Wundur shell integration (fish): marks prompts and commands.
status is-interactive; or exit
set -q WUNDUR_SHELL_INTEGRATION; or exit
set -q __wundur_loaded; and exit
set -g __wundur_loaded 1

function __wundur_esc
    string replace -a '\\' '\\\\' -- $argv | string replace -a ';' '\\x3b' | string join '\\x0a'
end

function __wundur_prompt --on-event fish_prompt
    printf '\e]133;A\a'
end

function __wundur_preexec --on-event fish_preexec
    printf '\e]633;E;%s\a\e]133;C\a' (__wundur_esc $argv)
end

function __wundur_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
end

printf '\e]633;P;Shell=fish\a'
"#;

const PWSH_RC: &str = r#"# Wundur shell integration (PowerShell): marks prompts and commands.
if (-not $Global:__WundurLoaded) {
  $Global:__WundurLoaded = $true
  $Global:__WundurRan = $false
  $Global:__WundurPrompt = $function:prompt
  function Global:__WundurEsc([string]$s) {
    $s.Replace('\', '\\').Replace(';', '\x3b').Replace("`r", '').Replace("`n", '\x0a')
  }
  function Global:prompt {
    $ok = $?
    $code = $global:LASTEXITCODE
    $e = [char]27; $a = [char]7
    $out = ''
    if ($Global:__WundurRan) {
      $n = if ($ok) { 0 } elseif ($code) { $code } else { 1 }
      $out += "$e]133;D;$n$a"
    }
    $Global:__WundurRan = $false
    $out += "$e]133;A$a" + (& $Global:__WundurPrompt) + "$e]133;B$a"
    $global:LASTEXITCODE = $code
    $out
  }
  if (Get-Module PSReadLine) {
    Set-PSReadLineKeyHandler -Chord Enter -ScriptBlock {
      $line = $null; $cursor = $null
      [Microsoft.PowerShell.PSConsoleReadLine]::GetBufferState([ref]$line, [ref]$cursor)
      [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
      if ($line.Trim()) {
        $Global:__WundurRan = $true
        $e = [char]27; $a = [char]7
        [Console]::Write("$e]633;E;$(__WundurEsc $line)$a$e]133;C$a")
      }
    }
  }
  [Console]::Write("$([char]27)]633;P;Shell=pwsh$([char]7)")
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!("wundur-si-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec(program: &str, args: &[&str]) -> CommandSpec {
        CommandSpec {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            env: Vec::new(),
        }
    }

    #[test]
    fn knows_shells_by_name() {
        assert_eq!(Shell::of(Path::new("/bin/zsh")), Some(Shell::Zsh));
        assert_eq!(Shell::of(Path::new("/opt/homebrew/bin/fish")), Some(Shell::Fish));
        assert_eq!(Shell::of(Path::new("powershell.exe")), Some(Shell::PowerShell));
        assert_eq!(Shell::of(Path::new("C:\\Program Files\\PowerShell\\7\\pwsh.exe")), Some(Shell::PowerShell));
        assert_eq!(Shell::of(Path::new("/bin/sh")), None);
        assert_eq!(Shell::of(Path::new("claude")), None);
    }

    #[test]
    fn other_programs_start_unchanged() {
        let dir = TempDir::new();
        assert!(integrate(&spec("/bin/sh", &["-l"]), dir.path()).is_none());
    }

    #[test]
    fn zsh_loads_through_its_own_startup_folder() {
        let dir = TempDir::new();
        let out = integrate(&spec("/bin/zsh", &["-l"]), dir.path()).unwrap();
        assert_eq!(out.args, vec!["-l"]);
        let zdotdir = out.env.iter().find(|(k, _)| k == "ZDOTDIR").unwrap();
        assert!(Path::new(&zdotdir.1).join(".zshrc").exists());
    }

    #[test]
    fn a_bash_login_shell_reads_the_login_files_itself() {
        let dir = TempDir::new();
        let out = integrate(&spec("/bin/bash", &["-l"]), dir.path()).unwrap();
        assert_eq!(out.args[0], "--rcfile");
        assert!(!out.args.iter().any(|a| a == "-l"));
        assert!(out.env.iter().any(|(k, _)| k == "WUNDUR_BASH_LOGIN"));
    }

    #[test]
    fn powershell_dot_sources_the_script() {
        let dir = TempDir::new();
        let out = integrate(&spec("pwsh", &[]), dir.path()).unwrap();
        assert_eq!(out.args[..2], ["-NoExit", "-Command"]);
        assert!(out.args[2].starts_with(". '"));
    }
}

/// Writes the scripts to `$WUNDUR_SI_OUT` and prints the specs, for trying them by hand.
#[cfg(test)]
#[test]
#[ignore]
fn write_for_manual_check() {
    let out = PathBuf::from(std::env::var("WUNDUR_SI_OUT").unwrap());
    for shell in ["/bin/zsh", "/bin/bash", "fish", "pwsh"] {
        let spec = CommandSpec { program: shell.into(), args: vec!["-l".into()], cwd: None, env: Vec::new() };
        println!("{:?}", integrate(&spec, &out).map(|s| (s.args, s.env)));
    }
}
