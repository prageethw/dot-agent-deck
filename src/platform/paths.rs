//! Home / runtime / state directory and IPC-endpoint path resolution
//! (PRD #42 M1, lifted from `config.rs`).
//!
//! The Unix branch preserves today's behavior byte-for-byte: `$HOME`,
//! `$XDG_RUNTIME_DIR`, `$XDG_CONFIG_HOME`, the per-uid `/tmp` socket fallback,
//! and `getuid(2)` namespacing. The Windows branch resolves
//! `%USERPROFILE%`/`%LOCALAPPDATA%`/`%APPDATA%` via the `dirs` crate and returns
//! named-pipe endpoint strings (`\\.\pipe\dot-agent-deck-{user}-…`, where
//! `{user}` is the current user's SID — see [`endpoint_user_suffix`]). The
//! `DOT_AGENT_DECK_*` env overrides stay authoritative on both platforms:
//! every resolver checks its override before consulting any platform default.
//!
//! Note: only the **path computation** lives here. The socket binding / I/O
//! that consumes these paths stays in `daemon*`/`hook`/`ui` until M2 abstracts
//! the transport.

use std::path::PathBuf;

/// Home directory used to anchor config/state/cache paths.
///
/// Unix: `$HOME`, falling back to `/` (matches the historical
/// `config::dirs_home`). Windows: `%USERPROFILE%`, falling back to `C:\`.
pub fn home_dir() -> PathBuf {
    #[cfg(unix)]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"))
    }
    #[cfg(windows)]
    {
        // `dirs::home_dir()` resolves `%USERPROFILE%` (via the known-folder API
        // — more robust than reading the env var directly).
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(r"C:\"))
    }
}

/// Home directory anchor for the **third-party tool config** writers —
/// `hooks_manage`'s `~/.claude/settings.json` and `opencode_manage`'s
/// `~/.config/opencode` / `~/.opencode` roots.
///
/// Identical to [`home_dir`] except for the Unix `$HOME`-unset fallback, which is
/// `/tmp` here instead of `/`. That is not a preference, it is preservation: both
/// call sites read `std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())`
/// before PRD #163 M1 routed them through this module, and #163's bar is
/// byte-for-byte Unix behavior — including in the `$HOME`-unset case, where the
/// paths would otherwise move from `/tmp/.claude/settings.json` to
/// `/.claude/settings.json` (a different file, and one an unprivileged user
/// cannot even create). The fallback lives here, at the seam, rather than as a
/// `cfg` in each call site.
///
/// Windows: exactly [`home_dir`] — `%USERPROFILE%` via the known-folder API. The
/// `/tmp` fallback has no meaning there (nothing loads `C:\tmp\.claude`), and
/// `$HOME` is normally unset on Windows, which is precisely why these two sites
/// had to come through the seam at all.
pub fn home_dir_with_tmp_fallback() -> PathBuf {
    #[cfg(unix)]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
    }
    #[cfg(windows)]
    {
        home_dir()
    }
}

/// Current real uid, used to namespace the `/tmp` fallback sockets per user.
/// Wraps `getuid(2)` so the single `unsafe` lives in one place.
///
/// Unix-only: Windows has no uid concept and namespaces its named-pipe
/// endpoints by username instead (see [`endpoint_user_suffix`]).
#[cfg(unix)]
pub fn current_uid() -> u32 {
    // SAFETY: `getuid(2)` is async-signal-safe and has no failure mode; it
    // simply returns the calling process's real uid.
    unsafe { libc::getuid() }
}

/// Per-user namespacing suffix for the Windows named-pipe endpoints — the
/// Win32 analogue of the per-uid `/tmp` socket suffix.
///
/// **PRD #163, release-gating.** The #42 skeleton read `%USERNAME%` and fell
/// back to the literal `"user"` when it was unset, which *collides across
/// users*: two accounts on one host would compute the same pipe name, so the
/// loser's clients would be handed to the winner's daemon. The uid this
/// replaces never collides, and neither may its Windows counterpart.
///
/// The source is therefore the **current user's SID** (`S-1-5-21-…`), read from
/// the process token — the exact analogue of `getuid(2)` — and not `%USERNAME%`:
///
/// - A SID is unique. `%USERNAME%` is not: `DOMAIN_A\alice` and `DOMAIN_B\alice`
///   logged into the same machine both report `alice`.
/// - A SID cannot be *steered*. An env var can, and the daemon, the ui/attach
///   client, and the hook client (which runs inside an agent's deliberately
///   scrubbed environment) must all derive the *same* endpoint name — an agent
///   whose `%USERNAME%` was unset or rewritten would otherwise compute a
///   different pipe name, or be pointed at a foreign one.
///
/// Resolved once and cached: a process's token user SID cannot change.
///
/// When the SID cannot be read there is no non-colliding source left, so this
/// is a hard error (per the PRD: "a non-colliding fallback **or hard error**")
/// rather than a silent collision. `DOT_AGENT_DECK_SOCKET` /
/// `DOT_AGENT_DECK_ATTACH_SOCKET` are consulted *before* this function and
/// bypass it entirely, so an explicit endpoint name is always available as an
/// escape hatch.
#[cfg(windows)]
pub fn endpoint_user_suffix() -> String {
    static SUFFIX: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SUFFIX
        .get_or_init(|| match current_user_sid() {
            Ok(sid) if is_pipe_name_token(&sid) => sid,
            Ok(sid) => panic!(
                "current-user SID {sid:?} is not usable as a named-pipe segment; \
                 refusing a colliding fallback — set DOT_AGENT_DECK_SOCKET and \
                 DOT_AGENT_DECK_ATTACH_SOCKET to explicit per-user pipe names"
            ),
            Err(err) => panic!(
                "cannot read the current user's SID for the per-user pipe name ({err}); \
                 refusing a colliding fallback — set DOT_AGENT_DECK_SOCKET and \
                 DOT_AGENT_DECK_ATTACH_SOCKET to explicit per-user pipe names"
            ),
        })
        .clone()
}

/// Whether `token` is safe to embed as the per-user segment of a
/// `\\.\pipe\dot-agent-deck-<token>-…` name: non-empty (an empty segment would
/// collide with every other empty one), restricted to characters that cannot
/// escape the pipe namespace (`\` is the pipe-name separator; `/` and whitespace
/// are rejected for the same reason), and short enough that the longest fixed
/// prefix+suffix around it (`\\.\pipe\dot-agent-deck-` + `-attach`, 31 chars)
/// still fits the 256-character named-pipe limit.
///
/// Compiled on every platform — it is pure data, so the rule stays unit-testable
/// on Linux CI where the `#[cfg(windows)]` caller is absent.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_pipe_name_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 200
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The current user's SID in canonical string form, cached, **without**
/// [`endpoint_user_suffix`]'s panic-on-failure (PRD #163 M4).
///
/// `endpoint_user_suffix` must panic when the SID is unreadable: it feeds a pipe
/// *name*, and the only alternative there is a colliding fallback. The filesystem
/// security backend has a better option — fail the individual operation closed —
/// so it needs the same one cached value as a `Result`. Both go through this,
/// which is what keeps the pipe name, the pipe's DACL, the spawn mutex's DACL and
/// the config-file DACLs from ever disagreeing about which user we are.
///
/// A process's token user SID cannot change, so the result (success *or* failure)
/// is resolved once. The error is cached as a string because [`std::io::Error`]
/// is not `Clone`; the kind is not load-bearing — every consumer treats "cannot
/// read our own SID" as fatal-for-this-operation.
#[cfg(windows)]
pub(crate) fn current_user_sid() -> std::io::Result<String> {
    static SID: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    match SID.get_or_init(|| current_user_sid_string().map_err(|err| err.to_string())) {
        Ok(sid) => Ok(sid.clone()),
        Err(message) => Err(std::io::Error::other(message.clone())),
    }
}

/// Read the calling process's user SID and return it in the canonical string
/// form (`S-<revision>-<authority>-<sub-authority>…`).
///
/// Uses the token rather than any env var so the value is identical in the
/// daemon and in every client, however their environments were scrubbed (see
/// [`endpoint_user_suffix`]).
#[cfg(windows)]
fn current_user_sid_string() -> std::io::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Closes the opened process token on every exit path below.
    struct TokenHandle(HANDLE);
    impl Drop for TokenHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from a successful `OpenProcessToken` and is
            // closed exactly once, here.
            unsafe { CloseHandle(self.0) };
        }
    }

    let mut raw_token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no release;
    // `raw_token` is a valid out-pointer for the duration of the call.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let token = TokenHandle(raw_token);

    // Documented size probe: a null buffer fails with ERROR_INSUFFICIENT_BUFFER
    // and reports the required byte count.
    let mut needed: u32 = 0;
    // SAFETY: null buffer + zero length is the probe form; `needed` is a valid
    // out-pointer.
    unsafe { GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(std::io::Error::last_os_error());
    }

    // `TOKEN_USER` leads with a pointer, so the buffer must be pointer-aligned;
    // a `Vec<u8>` is only byte-aligned. `Vec<u64>` is (over-)aligned for every
    // Windows target we build.
    let mut buf = vec![0u64; needed.div_ceil(8) as usize];
    // SAFETY: `buf` owns at least `needed` bytes of writable, 8-byte-aligned
    // storage, and `needed` is passed as its true length.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: on success the buffer holds a `TOKEN_USER` followed by the
    // variable-length SID it points into; both stay valid as long as `buf`.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };

    let mut wide: *mut u16 = std::ptr::null_mut();
    // SAFETY: `sid` is the token's SID and `wide` a valid out-pointer; on
    // success the callee hands back a `LocalAlloc`'d NUL-terminated string.
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut len = 0usize;
    // SAFETY: `wide` is NUL-terminated, so the scan stops inside the allocation.
    while unsafe { *wide.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` UTF-16 units of the live `LocalAlloc`'d buffer.
    let sid_string = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, len) });
    // SAFETY: frees exactly the buffer `ConvertSidToStringSidW` allocated;
    // `wide` is not read again.
    unsafe { LocalFree(wide.cast()) };

    drop(token);
    Ok(sid_string)
}

/// The crate's own package name — the literal fallback [`binary_name`] returns
/// when `current_exe()` is unavailable or unusable, and the single source of
/// truth every other such fallback in the crate should read rather than
/// re-typing the literal `"dot-agent-deck"`.
pub const DEFAULT_BINARY_NAME: &str = env!("CARGO_PKG_NAME");

/// The command name this build was invoked as — the file name component of
/// [`std::env::current_exe`] — for generated text that tells an agent to run
/// the deck **by name through `$PATH`** (the `delegate` / `work-done` CLI
/// examples in `orchestrator_context::build_orchestrator_context` and
/// `state::work_done_footer`). A build installed under a different file name
/// must generate instructions naming ITSELF, not a baked-in literal —
/// otherwise the generated command resolves to a different binary than the
/// one that wrote it.
///
/// **Symlink resolution is platform-dependent — this is a fact about the
/// platform, not a choice this function makes, and any doc comment asserting
/// a single cross-platform behavior here is wrong on one of the two.** On
/// macOS `current_exe()` is backed by `_NSGetExecutablePath`, which reports
/// the path the process was INVOKED as: a symlink stays a symlink, confirmed
/// directly (not assumed) with a four-way probe on this crate's dev machine
/// covering direct invocation, a same-directory symlink, an absolute-target
/// symlink in another directory, and `$PATH` lookup of a symlink name — all
/// four returned the symlink's own path, never the target. On Linux
/// `current_exe()` reads `/proc/self/exe`, which the kernel resolves fully: a
/// symlink returns its TARGET's path. So `~/bin/deck ->
/// /opt/x/dot-agent-deck` generates `deck` (still on `$PATH`) on macOS but
/// `dot-agent-deck` (possibly not on `$PATH` at all) on Linux, for the exact
/// same install.
///
/// Two gates keep the resolved name usable rather than merely well-formed
/// (issue #253 review/audit):
///
/// - **`$PATH`-resolvability.** The resolved file name is used ONLY when it
///   actually resolves via a `$PATH` lookup ([`resolves_on_path`]); otherwise
///   this falls back to [`DEFAULT_BINARY_NAME`]. `wrap.rs`'s
///   `deck_binary_for_wrap` already documents the policy this crate commits
///   to for "which name do I tell an agent to run": *"behaviour only ever
///   improves on what `$PATH` would have found."* Without this gate, a build
///   renamed but not installed on `$PATH` would regress from "resolves to
///   the wrong-but-runnable literal, by accident" to "resolves to nothing at
///   all" — trading a case that happened to work for one that silently never
///   does, which is precisely the failure this function exists to eliminate.
/// - **Shell safety.** A name outside [`is_safe_binary_name`]'s conservative
///   allowlist is rejected — not quoted — for the same reason `wrap.rs`'s
///   `usable()` rejects rather than quotes: this name is interpolated
///   UNQUOTED into ```` ```bash ```` blocks an agent executes verbatim, and
///   quoting an unsafe name would still resolve to nothing on a normal
///   `$PATH` — converting an injection into a silent no-op rather than a
///   name that at least works.
///
/// Falls back to [`DEFAULT_BINARY_NAME`] when `current_exe()` errors, its
/// file name is empty or not valid UTF-8, it fails the shell-safety check, or
/// it does not resolve on `$PATH`. The fallback matters more here than at
/// most other `current_exe()` call sites: `delegate` and `work-done` write to
/// the unversioned hook socket, both call sites are fire-and-forget, and the
/// daemon drops any frame it cannot parse without logging it — so a name
/// that resolves to a binary that cannot run produces no error anywhere,
/// only a signal that silently never arrives.
pub fn binary_name() -> String {
    resolve_binary_name(std::env::current_exe(), resolves_on_path)
}

/// Pure seam behind [`binary_name`]. `resolves_on_path` is injected so both
/// the malformed-input fallback ([`delegate/034`]) and the two usability
/// gates (shell safety, `$PATH` resolvability) are unit-testable with a
/// synthetic `current_exe()` and a synthetic resolver, without needing a
/// real unusable `current_exe()` or a real `$PATH` entry.
fn resolve_binary_name(
    current_exe: std::io::Result<PathBuf>,
    resolves_on_path: impl Fn(&str) -> bool,
) -> String {
    current_exe
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_os_string()))
        .and_then(|name| name.into_string().ok())
        .filter(|name| is_safe_binary_name(name))
        .filter(|name| resolves_on_path(name))
        .unwrap_or_else(|| DEFAULT_BINARY_NAME.to_string())
}

/// Whether `name` is safe to interpolate UNQUOTED into the generated `bash`
/// command examples [`binary_name`] feeds (issue #253 review F2 / audit F1):
/// a conservative ALLOWLIST rather than a denylist, since the failure mode
/// this guards against is an agent's shell reinterpreting whatever falls
/// outside it. Rejects an empty name, a leading `-` (would be read as a flag
/// by whatever runs the generated line), and anything outside ASCII
/// alphanumerics plus `-`, `_`, `.`, `+` — which also rejects the mundane
/// motivating cases (`dot-agent-deck (1)` from a browser download,
/// `dot-agent-deck copy` from a Finder duplicate) alongside the adversarial
/// ones (`;`, `` ` ``, `$`, a literal newline).
fn is_safe_binary_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+'))
}

/// Test-only override: when set to a non-empty value, [`binary_name`]'s
/// `$PATH`-resolvability gate ([`resolves_on_path`]) is treated as satisfied
/// unconditionally. Production never sets it.
///
/// `cargo test`/`cargo nextest` compiles each test into its own throwaway
/// binary under `target/<profile>/deps/`, which is never actually
/// discoverable on `$PATH` — so without this escape hatch, `binary_name()`
/// would ALWAYS take the fallback branch under test, which would make
/// `orchestration/delegate/032`/`033` (which assert that the RUNNING
/// binary's own name — not the fallback — propagates into generated text)
/// either vacuous or permanently red. Same pattern as `wrap.rs`'s
/// `DOT_AGENT_DECK_WRAP_BIN`; nextest gives each test its own process, so
/// setting this for one test's duration cannot leak into another.
pub const DOT_AGENT_DECK_TEST_BINARY_ON_PATH: &str = "DOT_AGENT_DECK_TEST_BINARY_ON_PATH";

/// Whether `name` resolves via a `$PATH` lookup — the real resolver
/// [`binary_name`] injects into [`resolve_binary_name`]. Checked with a bare
/// existence probe (`is_file()`), matching `wrap.rs`'s `usable()` precedent
/// rather than also requiring the execute bit: the value here is read-only
/// generated text, not something this process itself spawns.
fn resolves_on_path(name: &str) -> bool {
    if std::env::var(DOT_AGENT_DECK_TEST_BINARY_ON_PATH).is_ok_and(|v| !v.is_empty()) {
        return true;
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// Hook-ingestion endpoint. Unix: a Unix-domain-socket path
/// (`$XDG_RUNTIME_DIR/dot-agent-deck.sock` else `/tmp/dot-agent-deck-{uid}.sock`).
/// Windows: the named-pipe `\\.\pipe\dot-agent-deck-{user}-hook`, where
/// `{user}` is the non-colliding per-user token from [`endpoint_user_suffix`].
///
/// `DOT_AGENT_DECK_SOCKET` overrides on both platforms.
pub fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("DOT_AGENT_DECK_SOCKET") {
        return PathBuf::from(path);
    }

    #[cfg(unix)]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(runtime_dir).join("dot-agent-deck.sock");
        }

        // PRD #93 reviewer REV-2: the `/tmp` fallback must include the uid so
        // two users on the same host can't collide on the same socket path
        // (the daemon is per-user; the 0o600 mode is on the socket inode, but
        // the *path* still has to be unique, otherwise the loser's `bind(2)`
        // sees `EADDRINUSE` against the winner's inode). Same rationale as
        // `attach_socket_path` below.
        PathBuf::from(format!("/tmp/dot-agent-deck-{}.sock", current_uid()))
    }
    #[cfg(windows)]
    {
        PathBuf::from(format!(
            r"\\.\pipe\dot-agent-deck-{}-hook",
            endpoint_user_suffix()
        ))
    }
}

/// Streaming-attach endpoint (separate from the hook endpoint so the two
/// protocols have disjoint wire formats — hook is line-delimited JSON, attach
/// is a binary frame protocol). Unix: `$XDG_RUNTIME_DIR/dot-agent-deck-attach.sock`
/// else `/tmp/dot-agent-deck-attach-{uid}.sock`. Windows: the named pipe
/// `\\.\pipe\dot-agent-deck-{user}-attach` (`{user}` per
/// [`endpoint_user_suffix`]).
///
/// `DOT_AGENT_DECK_ATTACH_SOCKET` overrides on both platforms.
pub fn attach_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("DOT_AGENT_DECK_ATTACH_SOCKET") {
        return PathBuf::from(path);
    }

    #[cfg(unix)]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            return PathBuf::from(runtime_dir).join("dot-agent-deck-attach.sock");
        }

        // PRD #93 reviewer REV-2: include the uid in the `/tmp` fallback path so
        // two users on the same host get disjoint sockets (each daemon's
        // `bind(2)` would otherwise collide with the other user's inode), and
        // so the path itself can't be observed by another user to figure out
        // *which* deck process to target. The 0o600 mode on the inode is
        // already enforced; the per-user path is the missing half.
        PathBuf::from(format!("/tmp/dot-agent-deck-attach-{}.sock", current_uid()))
    }
    #[cfg(windows)]
    {
        PathBuf::from(format!(
            r"\\.\pipe\dot-agent-deck-{}-attach",
            endpoint_user_suffix()
        ))
    }
}

/// Per-user state directory (detached-daemon log, spawn mutex). Resolution
/// order on Unix:
///
/// 1. `DOT_AGENT_DECK_STATE_DIR` — explicit override (tests use this).
/// 2. `$XDG_STATE_HOME/dot-agent-deck` — freedesktop spec default.
/// 3. `$HOME/.local/state/dot-agent-deck` — XDG fallback.
///
/// Windows: the override first, then `%LOCALAPPDATA%\dot-agent-deck` (already
/// per-user ACL'd by default).
pub fn state_dir() -> PathBuf {
    if let Ok(path) = std::env::var("DOT_AGENT_DECK_STATE_DIR") {
        return PathBuf::from(path);
    }

    #[cfg(unix)]
    {
        match std::env::var("XDG_STATE_HOME") {
            Ok(state_home) if !state_home.is_empty() => {
                PathBuf::from(state_home).join("dot-agent-deck")
            }
            _ => home_dir().join(".local/state/dot-agent-deck"),
        }
    }
    #[cfg(windows)]
    {
        // `%LOCALAPPDATA%\dot-agent-deck` (already per-user ACL'd by default).
        state_dir_platform_root()
    }
}

/// Per-user **config** root — the directory holding `config.toml`,
/// `session.toml`, `keybindings.toml`, `remotes.toml`, `schedules.toml` and the
/// small JSON state files that live beside them (PRD #163 M1).
///
/// Unix: `$HOME/.config/dot-agent-deck` — byte-for-byte the historical
/// `dirs_home().join(".config/dot-agent-deck")` every caller used inline.
/// Windows: `%APPDATA%\dot-agent-deck` (`dirs::config_dir()`, resolved via the
/// known-folder API), falling back to `%USERPROFILE%\AppData\Roaming\…`.
/// `%APPDATA%` — not `%USERPROFILE%\.config` — is the conventional Windows
/// per-user config root, and it completes the `%LOCALAPPDATA%`/`%APPDATA%`/
/// `%USERPROFILE%` mapping locked in #42.
///
/// Every caller checks its own `DOT_AGENT_DECK_*` file override *before* calling
/// this, so those overrides stay authoritative on both platforms.
pub fn config_dir() -> PathBuf {
    #[cfg(unix)]
    {
        home_dir().join(".config/dot-agent-deck")
    }
    #[cfg(windows)]
    {
        match dirs::config_dir() {
            Some(config) => config.join("dot-agent-deck"),
            None => home_dir().join("AppData/Roaming/dot-agent-deck"),
        }
    }
}

/// `$XDG_CONFIG_HOME` when set and non-empty, else `None`.
///
/// Windows always returns `None`: the XDG spec has no Windows analogue, and
/// [`config_dir`] already resolves the platform's own per-user config root, so
/// only the `DOT_AGENT_DECK_*` overrides apply there. Keeping the check behind
/// this seam is what lets the callers stay `cfg`-free.
pub fn xdg_config_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        match std::env::var("XDG_CONFIG_HOME") {
            Ok(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
            _ => None,
        }
    }
    #[cfg(windows)]
    {
        None
    }
}

/// Platform default root for the daemon's per-endpoint lock files
/// (`{basename}-{hash}.lock`). Callers apply their own overrides — the
/// per-`Daemon` builder override and `DOT_AGENT_DECK_LOCK_DIR` — *before* this,
/// so this is only the platform tail of `daemon::lock_root`.
///
/// Unix: `$XDG_RUNTIME_DIR/dot-agent-deck` when set and non-empty, else
/// `$HOME/.cache/dot-agent-deck` — byte-for-byte the historical resolution.
/// Never `/tmp` (PRD #93 round-4 auditor BLOCKER: a world-writable lock root
/// lets a foreign uid pre-create the lock entry and DoS the target user's daemon
/// startup).
///
/// Windows: `%LOCALAPPDATA%\dot-agent-deck\locks` — there is no
/// `$XDG_RUNTIME_DIR` analogue, and `%LOCALAPPDATA%` carries the same
/// "not world-writable" property the Unix choice exists for. Kept distinct from
/// [`state_dir`] so the lock files stay separable from the daemon log / spawn
/// mutex.
pub fn lock_root_default() -> PathBuf {
    #[cfg(unix)]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR")
            && !runtime_dir.is_empty()
        {
            return PathBuf::from(runtime_dir).join("dot-agent-deck");
        }
        home_dir().join(".cache").join("dot-agent-deck")
    }
    #[cfg(windows)]
    {
        state_dir_platform_root().join("locks")
    }
}

/// `%LOCALAPPDATA%\dot-agent-deck` — the platform root shared by [`state_dir`]
/// and [`lock_root_default`] on Windows (the former uses it directly, the latter
/// nests `locks` under it). Split out so the `%LOCALAPPDATA%` fallback chain is
/// written once.
#[cfg(windows)]
fn state_dir_platform_root() -> PathBuf {
    match dirs::data_local_dir() {
        Some(local) => local.join("dot-agent-deck"),
        None => home_dir().join("AppData/Local/dot-agent-deck"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spec::spec;

    /// Scenario: Drive `resolve_binary_name` — the pure seam behind
    /// `binary_name` — directly with a synthetic `current_exe()` result for
    /// each unusable case a real call can produce: an `Err`, a path with no
    /// file name (`/`), and (Unix-only) a file name that is not valid UTF-8.
    /// Every case must fall back to `DEFAULT_BINARY_NAME`, never panic or
    /// produce an empty string.
    #[spec("orchestration/delegate/034")]
    #[test]
    fn delegate_034_binary_name_falls_back_to_the_default_literal_when_current_exe_is_unusable() {
        // The resolver is irrelevant to every case here — each fails before
        // `resolve_binary_name` would ever consult it — so an always-true
        // stub isolates that these are genuinely malformed-input failures,
        // not incidental `$PATH`/shell-safety rejections.
        assert_eq!(
            resolve_binary_name(Err(std::io::Error::other("no such process")), |_| true),
            DEFAULT_BINARY_NAME,
            "an current_exe() error must fall back to the default literal"
        );
        assert_eq!(
            resolve_binary_name(Ok(PathBuf::from("/")), |_| true),
            DEFAULT_BINARY_NAME,
            "a path with no file name component must fall back to the default literal"
        );
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            // 0xFF is not valid UTF-8 in any position, so `into_string()` fails.
            let invalid = OsStr::from_bytes(&[0xFF]);
            assert_eq!(
                resolve_binary_name(Ok(PathBuf::from("/usr/local/bin").join(invalid)), |_| true),
                DEFAULT_BINARY_NAME,
                "a non-UTF-8 file name must fall back to the default literal"
            );
        }
    }

    /// Reviewer finding F5: nothing previously pinned the SUCCESS branch, so
    /// a `resolve_binary_name` that returned the full path (instead of just
    /// the file name) would have passed the entire suite — every other test
    /// only exercises fallback inputs. This asserts the happy path returns a
    /// BARE file name, not an absolute path.
    #[test]
    fn resolve_binary_name_returns_the_bare_file_name_on_the_success_path() {
        assert_eq!(
            resolve_binary_name(Ok(PathBuf::from("/usr/local/bin/deck-x")), |_| true),
            "deck-x",
            "the success branch must return a bare file name, not the full path"
        );
    }

    /// Reviewer F2 / auditor F1: a well-formed name that WOULD resolve on
    /// `$PATH` must still fall back when it is not shell-safe — the
    /// shell-safety gate has to reject independently of the `$PATH` gate,
    /// not rely on an unsafe name also happening to be absent from `$PATH`.
    #[test]
    fn resolve_binary_name_falls_back_when_the_name_is_shell_unsafe() {
        assert_eq!(
            resolve_binary_name(
                Ok(PathBuf::from("/usr/local/bin/dot-agent-deck (1)")),
                |_| true
            ),
            DEFAULT_BINARY_NAME,
            "a name containing shell metacharacters must fall back even when it resolves \
             on $PATH"
        );
        assert_eq!(
            resolve_binary_name(
                Ok(PathBuf::from("/usr/local/bin/dot-agent-deck copy")),
                |_| true
            ),
            DEFAULT_BINARY_NAME,
            "a name containing whitespace must fall back (the Finder-duplicate case)"
        );
        assert_eq!(
            resolve_binary_name(Ok(PathBuf::from("/usr/local/bin/-rf")), |_| true),
            DEFAULT_BINARY_NAME,
            "a name with a leading '-' must fall back — it would be read as a flag"
        );
    }

    /// Reviewer F1 / auditor F1 (the pre-merge fix): a well-formed, shell-safe
    /// name that does NOT resolve on `$PATH` must fall back rather than
    /// emitting an unrunnable command — this is the case that regressed from
    /// "wrong but runnable by accident" to "resolves to nothing" before the
    /// gate existed.
    #[test]
    fn resolve_binary_name_falls_back_when_the_name_is_not_on_path() {
        assert_eq!(
            resolve_binary_name(Ok(PathBuf::from("/opt/build/worker-agent-deck")), |_| false),
            DEFAULT_BINARY_NAME,
            "a well-formed name that does not resolve on $PATH must fall back"
        );
    }

    /// The Windows per-user pipe segment must be a *non-colliding*, namespace-safe
    /// token (PRD #163). Pure data, so the rule is checked on Linux CI too.
    #[test]
    fn pipe_name_token_accepts_a_sid_and_rejects_unsafe_or_colliding_sources() {
        // The canonical SID string form — what `endpoint_user_suffix` embeds.
        assert!(is_pipe_name_token(
            "S-1-5-21-3623811015-3361044348-30300820-1013"
        ));
        assert!(is_pipe_name_token("alice"));

        // An empty segment collides with every other empty segment — exactly the
        // failure mode the literal `"user"` fallback had.
        assert!(!is_pipe_name_token(""));
        // `\` is the pipe-name separator: a domain-qualified name would escape
        // the `\\.\pipe\dot-agent-deck-…` namespace.
        assert!(!is_pipe_name_token(r"DOMAIN\alice"));
        assert!(!is_pipe_name_token("alice/../bob"));
        assert!(!is_pipe_name_token("first last"));
        assert!(!is_pipe_name_token("üser"));
        // Long enough to push the full pipe name past the 256-char limit.
        assert!(!is_pipe_name_token(&"a".repeat(201)));
    }

    /// The `DOT_AGENT_DECK_*` overrides are authoritative on BOTH platforms:
    /// they are consulted before any platform default, so a set override is
    /// returned verbatim (and, on Windows, short-circuits the per-user pipe-name
    /// derivation entirely).
    #[test]
    fn env_overrides_precede_every_platform_default() {
        let _guard = crate::config::STATE_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_socket = std::env::var("DOT_AGENT_DECK_SOCKET").ok();
        let prev_attach = std::env::var("DOT_AGENT_DECK_ATTACH_SOCKET").ok();
        let prev_state = std::env::var("DOT_AGENT_DECK_STATE_DIR").ok();
        // SAFETY: env-var lock held; every value is restored on the way out.
        unsafe {
            std::env::set_var("DOT_AGENT_DECK_SOCKET", "override-hook");
            std::env::set_var("DOT_AGENT_DECK_ATTACH_SOCKET", "override-attach");
            std::env::set_var("DOT_AGENT_DECK_STATE_DIR", "override-state");
        }

        assert_eq!(socket_path(), PathBuf::from("override-hook"));
        assert_eq!(attach_socket_path(), PathBuf::from("override-attach"));
        assert_eq!(state_dir(), PathBuf::from("override-state"));

        // SAFETY: same lock held; restoring the previous values.
        unsafe {
            match prev_socket {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_SOCKET", v),
                None => std::env::remove_var("DOT_AGENT_DECK_SOCKET"),
            }
            match prev_attach {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_ATTACH_SOCKET", v),
                None => std::env::remove_var("DOT_AGENT_DECK_ATTACH_SOCKET"),
            }
            match prev_state {
                Some(v) => std::env::set_var("DOT_AGENT_DECK_STATE_DIR", v),
                None => std::env::remove_var("DOT_AGENT_DECK_STATE_DIR"),
            }
        }
    }

    /// `config_dir` is the home-anchored config root, NOT an XDG-anchored one:
    /// only `schedules_path` honors `$XDG_CONFIG_HOME`, and it does so itself
    /// (via [`xdg_config_home`]) so that `config.toml`/`session.toml`/… keep
    /// their historical `~/.config/dot-agent-deck` location.
    #[cfg(unix)]
    #[test]
    fn config_dir_is_home_anchored_and_ignores_xdg_config_home() {
        let _guard = crate::config::STATE_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        // SAFETY: env-var lock held; restored on the way out.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "/should/not/anchor/config-dir");
        }

        assert_eq!(config_dir(), home_dir().join(".config/dot-agent-deck"));
        assert_eq!(
            xdg_config_home(),
            Some(PathBuf::from("/should/not/anchor/config-dir"))
        );

        // An empty value is treated as unset (the historical `!is_empty()` guard).
        // SAFETY: same lock held.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "");
        }
        assert_eq!(xdg_config_home(), None);

        // SAFETY: same lock held; restoring the previous value.
        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    /// PRD #163 review: the two third-party-tool config writers historically fell
    /// back to `/tmp` when `$HOME` was unset, and #163's bar is byte-for-byte Unix
    /// preservation — so the seam has to keep *two* fallbacks apart. With `$HOME`
    /// set both resolvers agree; with it unset `home_dir` yields `/` (the
    /// `config::dirs_home` behavior) and `home_dir_with_tmp_fallback` yields
    /// `/tmp` (the `hooks_manage`/`opencode_manage` behavior). Nothing asserted
    /// this before, which is how the regression got in.
    #[cfg(unix)]
    #[test]
    fn tool_config_home_keeps_the_historical_tmp_fallback() {
        let _guard = crate::config::STATE_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var("HOME").ok();

        // SAFETY: env-var lock held; restored on the way out.
        unsafe {
            std::env::set_var("HOME", "/home/somebody");
        }
        assert_eq!(home_dir(), PathBuf::from("/home/somebody"));
        assert_eq!(
            home_dir_with_tmp_fallback(),
            PathBuf::from("/home/somebody"),
            "with $HOME set the two resolvers must be identical"
        );

        // SAFETY: same lock held.
        unsafe {
            std::env::remove_var("HOME");
        }
        assert_eq!(home_dir(), PathBuf::from("/"));
        assert_eq!(
            home_dir_with_tmp_fallback(),
            PathBuf::from("/tmp"),
            "the tool-config resolver must keep the pre-#163 /tmp fallback"
        );

        // SAFETY: same lock held; restoring the previous value.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
