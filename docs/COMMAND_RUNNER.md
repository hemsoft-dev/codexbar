# Provider command collection

`SystemCommandRunner` runs usage-query CLIs with null stdin and separate capture
pipes. Native executables run directly. Windows batch shims use the system
`cmd.exe`, as Rust's `Command` does. Interactive sign-in flows use their separate
login adapters.

## Windows ordinary descendants

The Windows usage runner creates a new console with
`STARTF_USESHOWWINDOW` and `SW_HIDE` in `STARTUPINFOEXW`. The console is hidden
at creation. The runner never calls `ShowWindow`, changes the active desktop,
or modifies terminal settings. An ordinary console child and grandchild inherit
that console. Previously `CREATE_NO_WINDOW` left the provider root without a
console, allowing its ordinary console descendants to allocate visible consoles.
The previous flag was already present; this is a descendant inheritance repair.
See Microsoft's [creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags),
[console creation](https://learn.microsoft.com/en-us/windows/console/creation-of-a-console)
and [startup information](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/ns-processthreadsapi-startupinfow).

Only `SystemCommandRunner` uses this native launch path. It covers both the
Gemini version check and usage query, plus Copilot's background GitHub CLI calls.
The existing login, app-server and browser adapters retain their launch paths.
The native launcher inherits the caller's primary token, environment, working
directory and desktop. It supplies an explicit handle list containing null input
and the two write pipes. Process and thread handles remain noninheritable.
The provider starts suspended, enters the same kill-on-close job used before,
then resumes through its retained primary thread handle. Failure to establish
the job or resume terminates the suspended root. Native launch handles, attribute
storage and parent copies of child pipe handles have RAII owners.
See [process creation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw)
and [handle-list attributes](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute).

This boundary covers ordinary console inheritance. It does not prevent a CLI
from explicitly requesting a separate console, detaching, showing GUI windows,
or asking a separately running broker to show UI. It is not a hostile-process
UI sandbox. Windows Terminal/default-terminal configurations need separate
qualification before deployment. `GetConsoleWindow` can return a message-only
handle for a pseudoconsole while another terminal displays its output, per
[Microsoft's contract](https://learn.microsoft.com/en-us/windows/console/getconsolewindow).
An invisible handle alone is not proof that such a terminal is hidden.
Microsoft's public console-host
[handoff decision](https://github.com/microsoft/terminal/blob/98c8c795ebc2560af55e72beba3fa5fd31ceef64/src/server/IoDispatchers.cpp#L154)
declines delegation for `STARTF_USESHOWWINDOW` with `SW_HIDE` and for an already
initialized inherited console. That supports the design, but does not establish
parity with a deployed Windows console host or qualify a terminal configuration.

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
  pipe EOF and process exit. Idle checks sleep at most 25 ms. A timeout returns
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

The Windows console fixture starts a no-console caller of the real
`SystemCommandRunner`. Its provider root starts an ordinary child, which starts
an ordinary grandchild; neither descendant sets creation flags or visibility
hints. All three report console HWND, visibility samples, owner, window class,
console-process membership and desktop. GREEN requires one valid hidden
`ConsoleWindowClass` HWND shared by the full chain, matching owner and desktop,
each ancestor in the console-client list, and enumeration on that desktop.
Null, invalid, foreign and message-only HWNDs cannot pass as hidden-window proof.
It also verifies the caller's user SID, logon/session, working directory and a
synthetic inherited environment value, then waits for the console window to
disappear after the final client exits. These are read-only queries of native
fixtures. No desktop switch, terminal preference change or provider invocation
is used.

Additional Windows fixtures compare native argv and batch-shim behavior with
Rust's existing `Command`, count handles across repeated queries and abruptly
exit a runner with an owned descendant still running. The latter skips Rust
destructors and verifies Windows job-handle closure stops that descendant and
releases its locked resource. Existing finite-output, size-cap, stderr, exit,
null-input and timeout fixtures still run through the production collector.

The original launcher is retained in regression commit
[`0000dda`](https://github.com/hemsoft-dev/codexbar/commit/0000dda1e5b7a2cdeb23fe9c8d3fb48f4d1a822d)
for hosted RED evidence. Execution results and their exact revisions belong in
[PR #272](https://github.com/hemsoft-dev/codexbar/pull/272). Source inspection or
successful compilation alone does not establish RED or GREEN. This qualification
does not establish deployed-image parity, vendor re-exec behavior or game-crash
causality. Deployment remains a separate operator decision.
