# Provider command collection

`SystemCommandRunner` runs usage-query CLIs through the existing platform
program lookup. It does not invoke a shell, opens no console window, and closes
stdin. Interactive sign-in flows use their separate login adapters.

## Console windows

On Windows every background CLI starts with `CREATE_NO_WINDOW`, so it gets a
console with no window. Descendants that inherit that console have no window
either. A descendant started without a console, such as one detached from it,
gets a new, visible console that no flag on CodexBar's child can reach. The
Antigravity CLI's self-updater is one: it runs detached and starts
`agy --version`. `gemini_coding::CliRunner` therefore runs agy with Antigravity's
own `AGY_CLI_DISABLE_AUTO_UPDATE=true`
([troubleshooting](https://antigravity.google/docs/cli/troubleshooting/)). Only
`true` works ([antigravity-cli#1046](https://github.com/google-antigravity/antigravity-cli/issues/1046)).
`CommandOptions` without `cwd` keeps this process's working folder.

The `command_runner` fixture starts its child from a process with no console,
as the release app does. The child and an ordinary grandchild must both report
no console window.

## No interactive sign-in

A background CLI must never start a sign-in the user didn't ask for. Antigravity
uses a different token store over SSH: forcing `SSH_CONNECTION` hides the
Windows desktop keyring session (#289). `CliRunner` removes inherited SSH
variables and keeps HOME and USERPROFILE unchanged. It sets
`AGY_CLI_NONINTERACTIVE_HEADLESS=true`, overrides
`AGY_CLI_INTERACTIVE_HEADLESS=false`, and disables the self-updater.

Those headless flags alone do not stop the CLI frontend from attempting OAuth.
On Windows the probe therefore starts detached, without `CONIN$`, inside a job
with an active-process limit of one. The suspended CLI enters the job before
it runs, so it cannot launch its browser helper or any other child. Failure to
apply containment fails closed. The ordinary command runner and explicit
Settings sign-in keep their existing process policies. The probe stops at
`authentication required` on stderr and reports signed out. The version gate
requires 1.4.10, the build inspected for this auth behavior.

## Output and deadline policy

- Read stdout and stderr while the child runs. Each loop drains at most one
  8 KiB chunk from each stream, then checks completion and the deadline again.
  Neither stream can starve the other or keep an output loop alive indefinitely.
- Capture at most **8 MiB per stream**. Exceeding either limit returns
  `CommandError::Failed`, terminates the owned process tree and discards partial
  output. Bytes stay in memory. No payload is written to diagnostic files or logs.
- Decode UTF-8 after collection, preserving characters split across chunks and
  the previous replacement behavior for invalid UTF-8. Nonzero exit codes return
  `CommandOutput { success: false, .. }` with the collected output.
- The supplied timeout starts after successful process creation and covers both
  pipe EOF and process exit. An idle check waits at most 25 ms: on Unix it
  `poll`s the open pipes and returns as soon as one has output or closes, so a
  writer that refills a small pipe (16 KiB on macOS) isn't read one fixed nap at
  a time (#281); on Windows, whose anonymous pipes can't be polled, and once
  both pipes have closed, it sleeps. A timeout returns
  `TimedOut` after cleanup; cleanup failure returns `Failed` instead of claiming
  that termination completed. Process creation itself is a native system call.
- Verify owned tree termination for up to one additional second. Stop and reap
  the root on every return path, including setup, read and output-limit failures.
  OS scheduling and native process creation/termination calls are outside a
  real-time scheduling guarantee.

There are **no reader threads or asynchronous reader tasks** to detach or join.
On Unix the parent uses nonblocking anonymous-pipe reads. On Windows it queries
available bytes before reading, with exclusive ownership of each read handle.
No other thread can consume the reported bytes or serialize an overlapping read
on that synchronous handle. See Microsoft's
[`PeekNamedPipe` contract](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe).

## Process ownership

Windows reuses the existing suspended-start, kill-on-close Job Object, so the CLI
and its descendants enter the job before running. Explicit cleanup snapshots the
job's process IDs, opens each process with terminate/query/synchronize rights,
and verifies that the retained handle belongs to this job before stopping it.
It waits for each process handle to become signaled, then rescans for descendants
created during cleanup. All scans and waits share one second; snapshots are
bounded to 4096 members and fail closed on truncation. PID reuse cannot redirect
termination outside this job. Kill-on-close remains the fallback if verification
fails. Job accounting alone is not used as a termination barrier. See the
[job process-list contract](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_process_id_list)
and Microsoft's
[process-termination contract](https://learn.microsoft.com/en-us/windows/win32/procthread/terminating-a-process).

Unix query commands start in their own process group. Exit observation uses
[`waitid` with `WNOWAIT`](https://pubs.opengroup.org/onlinepubs/9699919799/functions/waitid.html),
keeping the root unreaped until the owned group is signalled. This reserves the
PID and prevents a reused group ID from receiving a kill signal. After reaping,
termination verification never sends another kill. Linux first queries process
group membership and reads `/proc/<pid>/stat` only for members of the owned
group. ENOENT or ESRCH while reading a member's record means it disappeared;
other read errors fail cleanup. It distinguishes live states from zombie/dead
states, so delayed reaping
by PID 1 or a subreaper does not turn a completed command into a failure. The
scan shares the one-second cleanup deadline. macOS uses libproc to list the
owned group's members and inspect their BSD process states, also excluding
zombies through Apple's
[libproc queries](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.c).
Its snapshot is bounded to fewer than 4096 members and fails closed if
that bound is reached. Both platforms recheck group membership against PID reuse;
neither requires somebody else's reaper to remove an already terminated record.
Normal descendants in the job/group are cleaned up even if the root exits first
or a descendant redirects its output. A deliberately escaping Unix process that
creates a new session is outside this process-group contract.

The collector keeps the same principle as Rust's
[`Child::wait_with_output`](https://doc.rust-lang.org/std/process/struct.Child.html#method.wait_with_output):
read both streams while waiting, rather than waiting for exit before reading.
Its explicit deadline and output cap also cover a descendant holding a pipe open.

## Credential-free qualification

```sh
cargo test -p codexbar-providers --test command_runner --locked
```

The test executable launches itself as a native fixture, never an installed
provider CLI. It checks exact 1 MiB stdout-only, stderr-only and simultaneous
output, the 8 MiB boundary on both streams, overflow rejection, UTF-8 across a
chunk boundary, invalid UTF-8, nonzero exit, null stdin and missing executables.
It also checks a hung root, an exited root whose descendant keeps pipes open,
and a descendant with redirected output. On Windows, redirecting the standard
streams does not stop Rust's process spawn from inheriting other inheritable
handles. The redirected-output fixture clears `HANDLE_FLAG_INHERIT` on its
captured handles before spawning its descendant. A separate Windows control
leaves those handles inheritable despite null standard streams and must time
out. Both paths still require confirmed process termination and resource release.
See [Rust's Windows spawn implementation](https://github.com/rust-lang/rust/blob/main/library/std/src/sys/process/windows.rs)
and [Microsoft's handle-inheritance flags](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-sethandleinformation).
A separate atomically published ready
file proves that each fixture acquired its resource before the test proceeds.
The test never reads a file while the fixture holds its exclusive lock. On
Windows and macOS it first verifies the fixture process's terminated/absent
state, then allows up to one second for the OS to release a file lock. This
separation also catches a macOS exit/lock-release race observed during the full
workspace run. Microsoft documents that automatic
[file unlocking can lag process termination](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex).
Other platforms require immediate reacquisition after cleanup. The product's
job/group exit verification stays independent of this filesystem oracle.

A native Unix unit test leaves an exited owned child unreaped while verifying
that cleanup accepts its terminated state. On Linux it also proves the zombie's
group remains visible to signal zero. Process-state fixtures cover zombie/dead
versus sleeping/running/stopped members, PID reuse, disappearing records, errors
and the scan deadline. Deadlines include a bounded cleanup margin.

Linux can also return a successful stat read with process group `-1` during
teardown. The [kernel's stat producer](https://github.com/torvalds/linux/blob/master/fs/proc/array.c)
initializes that sentinel before trying to lock the task's signal group. The
state character was sampled earlier, so it can still describe a running task.
That snapshot no longer identifies a member of the owned group. The parser
accepts the exact `-1` sentinel, while other invalid group fields remain errors.

Workspace tests and CI run this harness on Windows, macOS and Linux. No account,
credential store, browser, desktop input or live provider response is involved.
