//! Cooperative, owned-only native control. Its command/generation handoff is NOT a console/vendor contract.
//! The actual callback determines the event thread; no generator tuple is fabricated.
//! Notify only after SHOW: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-notifywinevent
//! Creator readback: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowthreadprocessid
use super::*;
#[path = "binding_policy.rs"]
mod policy;
use policy::{Actor, Binding, Registration, Ticket, Witness};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};
use windows::Win32::Foundation::WAIT_FAILED;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetProcessIdOfThread, GetThreadTimes, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SYNCHRONIZE,
};
use windows::Win32::UI::Accessibility::NotifyWinEvent;
use windows::Win32::UI::WindowsAndMessaging::OBJID_WINDOW;

fn thread_birth(handle: HANDLE) -> Result<u64> {
    let (mut born, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    unsafe { GetThreadTimes(handle, &mut born, &mut exit, &mut kernel, &mut user) }
        .map_err(|e| failure("GetThreadTimes owned control", e))?;
    Ok(ticks(born))
}
fn actor() -> Result<Actor> {
    Ok(Actor {
        pid: std::process::id(),
        process_birth: creation(unsafe { GetCurrentProcess() })?,
        tid: unsafe { GetCurrentThreadId() },
        thread_birth: thread_birth(unsafe { GetCurrentThread() })?,
    })
}
fn actor_json(a: Actor) -> Value {
    json!({"pid": a.pid, "process_birth": a.process_birth, "tid": a.tid, "thread_birth": a.thread_birth})
}
fn progress_set(progress: &Mutex<Value>, key: &str, value: Value) -> Result<()> {
    progress.lock().map_err(|_| "owned receipt mutex poisoned")?[key] = value;
    Ok(())
}
struct RetainedActor {
    identity: Actor,
    thread: OwnedHandle,
    process: OwnedHandle,
}
impl RetainedActor {
    fn retain(a: Actor) -> Result<Self> {
        if a.pid != std::process::id() || a.process_birth != creation(unsafe { GetCurrentProcess() })? {
            return Err("only this cooperative fixture process may register".into());
        }
        let raw = unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION | THREAD_SYNCHRONIZE, false, a.tid) }
            .map_err(|e| failure("OpenThread registered owned actor", e))?;
        let thread = unsafe { OwnedHandle::from_raw_handle(raw.0) };
        let process = open(a.pid)?;
        let retained = Self {
            identity: a,
            thread,
            process,
        };
        retained.live()?;
        Ok(retained)
    }
    fn live(&self) -> Result<Actor> {
        let thread = HANDLE(self.thread.as_raw_handle());
        let process = HANDLE(self.process.as_raw_handle());
        for handle in [thread, process] {
            let wait = unsafe { WaitForSingleObject(handle, 0) };
            if wait == WAIT_FAILED {
                return Err(format!(
                    "owned actor WaitForSingleObject: {}",
                    unsafe { GetLastError() }.0
                ));
            }
            if wait != WAIT_TIMEOUT {
                return Err("registered actor exited before cooperative handoff completed".into());
            }
        }
        let pid = unsafe { GetProcessIdOfThread(thread) };
        if pid == 0 {
            return Err(format!(
                "owned actor GetProcessIdOfThread: {}",
                unsafe { GetLastError() }.0
            ));
        }
        let actual = Actor {
            pid,
            process_birth: creation(process)?,
            tid: self.identity.tid,
            thread_birth: thread_birth(thread)?,
        };
        if actual != self.identity {
            return Err("retained process/thread creation identity differs".into());
        }
        Ok(actual)
    }
    fn wait_exited(&self, deadline: Instant) -> Result<()> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("owned thread cleanup deadline")?;
        let wait = unsafe { WaitForSingleObject(HANDLE(self.thread.as_raw_handle()), remaining.as_millis() as u32) };
        if wait == WAIT_FAILED {
            return Err(format!(
                "owned thread exit WaitForSingleObject: {}",
                unsafe { GetLastError() }.0
            ));
        }
        if wait != WAIT_OBJECT_0 {
            return Err("owned control thread did not terminate within fixture deadline".into());
        }
        if thread_birth(HANDLE(self.thread.as_raw_handle()))? != self.identity.thread_birth {
            return Err("thread creation identity changed after wait".into());
        }
        Ok(())
    }
}
fn window_creator(hwnd: HWND, creator: &RetainedActor) -> Result<Actor> {
    let mut pid = 0;
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if tid == 0 {
        return Err(format!(
            "live control GetWindowThreadProcessId: {}",
            unsafe { GetLastError() }.0
        ));
    }
    let actor = creator.live()?;
    if pid != actor.pid || tid != actor.tid {
        return Err("window creator readback differs from retained owned actor".into());
    }
    Ok(actor)
}
fn gone(hwnd: HWND) -> Result<Value> {
    let mut pid = 0;
    unsafe { SetLastError(WIN32_ERROR(0)) };
    let tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let error = if tid == 0 {
        Some(unsafe { GetLastError() }.0)
    } else {
        None
    };
    if tid != 0 || pid != 0 || unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err("controlled HWND destruction not independently confirmed".into());
    }
    Ok(json!({"window_query_return": tid, "window_query_error": error, "is_window": false}))
}
fn receive<T>(rx: &Receiver<T>, deadline: Instant) -> Result<T> {
    loop {
        if Instant::now() >= deadline {
            return Err("owned lifecycle control deadline".into());
        }
        match rx.try_recv() {
            Ok(value) => return Ok(value),
            Err(mpsc::TryRecvError::Disconnected) => return Err("owned control channel disconnected".into()),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let mut message = MSG::default();
        let mut count = 0;
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            count += 1;
            if count > CAP {
                return Err("control message cap".into());
            }
            unsafe { DispatchMessageW(&message) };
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[derive(Clone, Copy)]
enum Generate {
    Notify { hwnd: u64, kind: u32 },
    Stop,
}
enum Request {
    Register(Registration),
    Arm(Ticket),
    Pause,
    Destroyed { nonce: u64, generation: u64 },
    Resume,
    Finish,
}
enum Response {
    Ack(&'static str),
    Mapped(u32),
    Finished(Value),
    Failure(String),
}
fn response(rx: &Receiver<Response>, deadline: Instant, expected: &str) -> Result<Option<Value>> {
    match receive(rx, deadline)? {
        Response::Ack(label) if label == expected => Ok(None),
        Response::Mapped(0x8002) if expected == "show" => Ok(None),
        Response::Mapped(0x8001) if expected == "destroy" => Ok(None),
        Response::Finished(value) if expected == "finished" => Ok(Some(value)),
        Response::Failure(error) => Err(error),
        _ => Err("unexpected owned lifecycle protocol response".into()),
    }
}
fn control_observer(start: Instant, expected: Arc<AtomicUsize>, desktop: String) -> Result<Observer> {
    // Real resolver state; only bookkeeping initialization is shared in shape, no ownership decision is copied.
    Ok(Observer {
        directory: PathBuf::new(),
        start,
        anchor: std::process::id(),
        anchor_born: creation(unsafe { GetCurrentProcess() })?,
        session: session(std::process::id())?,
        phase: Arc::new(AtomicU8::new(1)),
        control_hwnd: expected,
        stop: Arc::new(AtomicBool::new(false)),
        processes: BTreeMap::new(),
        events: Vec::new(),
        errors: Vec::new(),
        desktop,
        control_seen: false,
        max_gap_us: 0,
        last: Instant::now(),
        scans: 0,
        cap_hit: false,
        hook: None,
        coverage_start: 0,
        hook_control_seen: false,
        hook_control_destroyed: false,
        unresolved_events: 0,
        unresolved_api_errors: BTreeMap::new(),
        max_hook_lag_ms: 0,
        known: BTreeMap::new(),
        image: String::new(),
        diagnostics: Diagnostics::new(),
    })
}
fn negatives(r: Registration, t: Ticket, w: Witness) -> Result<Value> {
    let mut outcomes = Vec::new();
    for (label, ticket, witness) in [
        (
            "unknown-hwnd",
            t,
            Witness {
                hwnd: r.hwnd.wrapping_add(1),
                ..w
            },
        ),
        (
            "wrong-generation",
            Ticket {
                generation: r.generation.wrapping_add(1),
                ..t
            },
            w,
        ),
        (
            "wrong-nonce",
            Ticket {
                nonce: r.nonce.wrapping_add(1),
                ..t
            },
            w,
        ),
        (
            "wrong-thread-birth",
            t,
            Witness {
                generator: Actor {
                    thread_birth: w.generator.thread_birth.wrapping_add(1),
                    ..w.generator
                },
                ..w
            },
        ),
        (
            "wrong-generator",
            t,
            Witness {
                generator: r.creator,
                ..w
            },
        ),
        (
            "missing-live-owner",
            t,
            Witness {
                live_creator: None,
                ..w
            },
        ),
    ] {
        let rejected = Binding::register(r, r.creator)
            .map_err(str::to_string)?
            .map(ticket, witness)
            .is_err();
        if !rejected {
            return Err(format!("cooperative negative control accepted: {label}"));
        }
        outcomes.push(json!({"case": label, "rejected": rejected}));
    }
    Ok(
        json!({"basis": "policy perturbations of a measured owned tuple, not observed OS events or recycled HWNDs", "outcomes": outcomes}),
    )
}
fn worker(
    start: Instant,
    expected: Arc<AtomicUsize>,
    requests: Receiver<Request>,
    tx: &Sender<Response>,
    ready: &mpsc::SyncSender<Result<Actor>>,
    progress: &Mutex<Value>,
) -> Result<()> {
    let desktop = thread_desktop(unsafe { GetCurrentThreadId() })?;
    let input = input_desktop()?;
    let station = station::current();
    let session = session(std::process::id())?;
    progress_set(
        progress,
        "ready_context",
        json!({"desktop": desktop, "input_desktop": input, "session": session, "station": station}),
    )?;
    if desktop != input || station["visible"] != true || session == 0 {
        return Err("binding observer is not on the interactive input desktop".into());
    }
    let mut hook = super::super::hooks::Hook::install()?;
    let mut resolver = control_observer(start, expected, desktop.clone())?;
    let mut anonymous_foreign = super::super::hooks::pump()?.len();
    let mut event_count = anonymous_foreign;
    ready.send(Ok(actor()?)).map_err(|e| e.to_string())?;
    let mut registration: Option<Registration> = None;
    let mut actors: Option<(RetainedActor, RetainedActor)> = None;
    let mut binding: Option<Binding> = None;
    let mut pending: Option<Ticket> = None;
    let mut paused = true; // Events remain queued until live registration is read back independently.
    let mut events = Vec::new();
    let mut show = false;
    let mut destroy = false;
    let mut legacy_show_rejected = false;
    let mut legacy_destroy_accepted = false;
    let mut native_wrong_generator_rejected = false;
    let mut unknown_for_registered_hwnd = 0;
    let mut negative = Value::Null;
    let mut destroyed_readback = Value::Null;
    while start.elapsed() < Duration::from_secs(10) {
        match requests.try_recv() {
            Ok(Request::Register(r)) => {
                if registration.is_some() {
                    return Err("window generation was registered twice".into());
                }
                let creator = RetainedActor::retain(r.creator)?;
                let generator = RetainedActor::retain(r.generator)?;
                let readback = window_creator(HWND(r.hwnd as *mut _), &creator)?;
                binding = Some(Binding::register(r, readback).map_err(str::to_string)?);
                resolver.record(HWND(r.hwnd as *mut _))?;
                progress_set(
                    progress,
                    "independent_registration_readback",
                    json!({"creator": actor_json(readback),
                    "generator": actor_json(generator.live()?), "window": resolver.events.last()}),
                )?;
                registration = Some(r);
                actors = Some((creator, generator));
                paused = false;
                tx.send(Response::Ack("registered")).map_err(|e| e.to_string())?;
            }
            Ok(Request::Arm(ticket)) => {
                if pending.is_some() {
                    return Err("overlapping cooperative notifications".into());
                }
                pending = Some(ticket);
                tx.send(Response::Ack("armed")).map_err(|e| e.to_string())?;
            }
            Ok(Request::Pause) => {
                paused = true;
                tx.send(Response::Ack("paused")).map_err(|e| e.to_string())?;
            }
            Ok(Request::Destroyed { nonce, generation }) => {
                if !paused {
                    return Err("destroy handoff must precede delayed event delivery".into());
                }
                let r = registration.ok_or("destroy without registration")?;
                destroyed_readback = gone(HWND(r.hwnd as *mut _))?;
                progress_set(progress, "destroy_readback", destroyed_readback.clone())?;
                binding
                    .as_mut()
                    .ok_or("missing cooperative registry")?
                    .confirm_destroyed(nonce, generation)
                    .map_err(str::to_string)?;
                tx.send(Response::Ack("destroyed")).map_err(|e| e.to_string())?;
            }
            Ok(Request::Resume) => {
                paused = false;
                tx.send(Response::Ack("resumed")).map_err(|e| e.to_string())?;
            }
            Ok(Request::Finish) => {
                let (creator, generator) = actors.as_ref().ok_or("missing retained actors")?;
                creator.live()?;
                generator.live()?;
                if !show
                    || !destroy
                    || !legacy_show_rejected
                    || !legacy_destroy_accepted
                    || !native_wrong_generator_rejected
                    || unknown_for_registered_hwnd != 0
                {
                    return Err("binding validation incomplete; no original/candidate pair permitted".into());
                }
                let tail = super::super::hooks::pump()?;
                event_count += tail.len();
                progress_set(progress, "event_count", json!(event_count))?;
                if event_count > CAP {
                    progress_set(progress, "event_cap_hit", json!(true))?;
                    return Err("owned binding cleanup event cap exceeded".into());
                }
                let registered_hwnd = registration.ok_or("missing registration at cleanup")?.hwnd;
                if tail.iter().any(|event| event.hwnd.0 as u64 == registered_hwnd) {
                    return Err("registered numeric HWND event remained queued at binding cleanup".into());
                }
                anonymous_foreign += tail.len();
                hook.close()?;
                let result = json!({"events": events, "native_show_mapped": show, "native_delayed_destroy_mapped": destroy,
                    "legacy_resolver_show_rejected": legacy_show_rejected, "legacy_resolver_destroy_accepted": legacy_destroy_accepted,
                    "native_wrong_generator_rejected": native_wrong_generator_rejected,
                    "destroy_readback": destroyed_readback, "negative_policy_controls": negative,
                    "anonymous_foreign_events": anonymous_foreign, "unattributable_registered_numeric_hwnd_events": unknown_for_registered_hwnd,
                    "event_count": event_count, "event_cap": CAP, "event_cap_hit": false,
                    "hook_closed": true, "event_loss": false, "queue_drained": true, "desktop": desktop,
                    "window_generation_source": "explicit fixture registration and serialized command handoff only",
                    "provider_or_broker_generation_proof": false});
                {
                    let mut saved = progress.lock().map_err(|_| "owned receipt mutex poisoned")?;
                    for (key, value) in result.as_object().ok_or("invalid result receipt")? {
                        saved[key] = value.clone();
                    }
                }
                tx.send(Response::Finished(
                    progress.lock().map_err(|_| "owned receipt mutex poisoned")?.clone(),
                ))
                .map_err(|e| e.to_string())?;
                return Ok(());
            }
            Err(mpsc::TryRecvError::Disconnected) => return Err("binding controller exited".into()),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if !paused {
            let r = registration.ok_or("pump without owned registration")?;
            let (creator, generator) = actors.as_ref().ok_or("missing retained actors")?;
            let batch = super::super::hooks::pump()?;
            event_count += batch.len();
            progress_set(progress, "event_count", json!(event_count))?;
            if event_count > CAP {
                progress_set(progress, "event_cap_hit", json!(true))?;
                return Err("owned binding total event cap exceeded".into());
            }
            for event in batch {
                if event.hwnd.0 as u64 != r.hwnd {
                    anonymous_foreign += 1;
                    progress_set(progress, "anonymous_foreign_events", json!(anonymous_foreign))?;
                    continue;
                }
                if event.thread != r.creator.tid && event.thread != r.generator.tid {
                    unknown_for_registered_hwnd += 1;
                    progress_set(
                        progress,
                        "unattributable_registered_numeric_hwnd_events",
                        json!(unknown_for_registered_hwnd),
                    )?;
                    continue;
                }
                let creator_readback = creator.live()?;
                let generator_readback = generator.live()?;
                if events.len() >= 64 {
                    return Err("owned binding event cap".into());
                }
                progress_set(
                    progress,
                    "last_owned_hook_tuple_before_resolution",
                    json!({"hwnd": r.hwnd,
                    "creator": actor_json(r.creator), "registered_generator": actor_json(r.generator),
                    "actual_event_thread": event.thread, "kind": event.kind, "callback_sequence": event.sequence,
                    "generated_ms": event.generated_ms, "received_ms": event.received_ms,
                    "fixture_generation": r.generation, "cooperative_mapping": "pending", "original_visibility_red": false}),
                )?;
                // Exercise the SAME resolver body used by the provider observer, with a real hook tuple.
                let old = resolver.resolve_events(vec![event]);
                progress_set(progress, "last_existing_resolver_error", json!(old.as_ref().err()))?;
                let mut mapping = "no-cooperative-ticket-for-this-generator";
                if let Some(ticket) = pending
                    && event.kind == ticket.kind
                    && event.thread
                        == if ticket.kind == EVENT_OBJECT_DESTROY {
                            r.creator.tid
                        } else {
                            r.generator.tid
                        }
                {
                    let live_creator = if event.kind == EVENT_OBJECT_DESTROY {
                        gone(event.hwnd)?;
                        None
                    } else {
                        Some(window_creator(event.hwnd, creator)?)
                    };
                    let witness = Witness {
                        hwnd: r.hwnd,
                        kind: event.kind,
                        callback_sequence: event.sequence,
                        generator: if event.thread == r.generator.tid {
                            generator_readback
                        } else {
                            creator_readback
                        },
                        live_creator,
                    };
                    if event.kind == EVENT_OBJECT_SHOW {
                        negative = negatives(r, ticket, witness)?;
                        progress_set(progress, "negative_policy_controls", negative.clone())?;
                    }
                    binding
                        .as_mut()
                        .ok_or("missing binding")?
                        .map(ticket, witness)
                        .map_err(str::to_string)?;
                    if event.kind == EVENT_OBJECT_SHOW {
                        if old.as_ref().err().map(String::as_str)
                            != Some("WinEvent identity differs from retained window owner/thread")
                        {
                            return Err(
                                "real resolver did not reach the expected creator/generator identity rejection".into(),
                            );
                        }
                        show = true;
                        legacy_show_rejected = true;
                    } else if event.kind == EVENT_OBJECT_DESTROY {
                        if old.is_err() {
                            return Err("existing resolver rejected the retained creator's delayed DESTROY".into());
                        }
                        destroy = true;
                        legacy_destroy_accepted = true;
                    }
                    mapping = "cooperative-fixture-command-generation-only";
                    pending = None;
                    tx.send(Response::Mapped(event.kind)).map_err(|e| e.to_string())?;
                } else {
                    if old.is_err() {
                        return Err("creator-generated native control event was rejected".into());
                    }
                    if let Some(ticket) = pending
                        && event.kind == ticket.kind
                    {
                        let wrong_source = Witness {
                            hwnd: r.hwnd,
                            kind: event.kind,
                            callback_sequence: event.sequence,
                            generator: creator_readback,
                            live_creator: Some(window_creator(event.hwnd, creator)?),
                        };
                        if binding.as_mut().ok_or("missing binding")?.map(ticket, wrong_source)
                            != Err("wrong-generator-identity")
                        {
                            return Err("real creator event was not rejected for the separate-generator ticket".into());
                        }
                        native_wrong_generator_rejected = true;
                        mapping = "rejected-real-creator-event-for-separate-generator-ticket";
                        progress_set(progress, "native_wrong_generator_rejected", json!(true))?;
                    }
                }
                events.push(json!({"phase": "owned-lifecycle-binding-control", "hwnd": r.hwnd, "creator_tid": r.creator.tid,
                    "generator_tid": event.thread, "generator_thread_birth": if event.thread == r.generator.tid { r.generator.thread_birth } else { r.creator.thread_birth },
                    "registered_pid": r.creator.pid, "process_birth": r.creator.process_birth,
                    "fixture_generation": r.generation, "event_kind": event.kind, "event_generated_ms": event.generated_ms,
                    "event_received_ms": event.received_ms, "callback_sequence": event.sequence, "mapping": mapping,
                    "existing_resolver_error": old.err(), "original_visibility_red": false}));
                progress_set(progress, "events", json!(events))?;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Err("owned binding fixture exceeded ten-second deadline".into())
}

pub(super) fn run() {
    let mut report = json!({"result": "owned-lifecycle-binding", "complete": false, "failure": null,
        "original_visibility_red": false, "scope": "cooperative-owned-fixture-only", "provider_or_broker_generation_proof": false,
        "cleanup_confirmed": false});
    let progress = Arc::new(Mutex::new(json!({"phase": "owned-lifecycle-binding-control",
        "events": [], "provider_or_broker_generation_proof": false, "hook_closed": false,
        "event_loss": "unknown", "queue_drained": false, "event_count": 0, "event_cap": CAP,
        "event_cap_hit": false, "coverage": "incomplete until cooperative receipt and retained cleanup"})));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(10);
        let creator = actor()?;
        report["creator"] = actor_json(creator);
        let retained_creator = RetainedActor::retain(creator)?;
        let mut members = [0; 16];
        let count = unsafe { GetConsoleProcessList(&mut members) };
        let console_error = if count == 0 {
            Some(unsafe { GetLastError() }.0)
        } else {
            None
        };
        let console_hwnd = unsafe { GetConsoleWindow() }.0 as usize;
        report["caller_console_query"] = json!({"return": count, "error": console_error, "success": count != 0,
            "console_hwnd": console_hwnd, "creation_contract": "DETACHED_PROCESS synthetic caller"});
        if count != 0 || console_hwnd != 0 {
            return Err("binding control caller must be detached/consoleless by creation contract".into());
        }
        let expected = Arc::new(AtomicUsize::new(0));
        let (requests_tx, requests_rx) = mpsc::channel();
        let (responses_tx, responses_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let control = expected.clone();
        let worker_progress = progress.clone();
        let observer_thread = std::thread::spawn(move || {
            let result = worker(start, control, requests_rx, &responses_tx, &ready_tx, &worker_progress);
            if let Err(error) = &result {
                let _ = ready_tx.try_send(Err(error.clone()));
                let _ = responses_tx.send(Response::Failure(error.clone()));
            }
            result
        });
        let observer_actor = receive(&ready_rx, (start + READY).min(deadline))??;
        let observer_retained = RetainedActor::retain(observer_actor)?;
        report["ready_us"] = json!(start.elapsed().as_micros());
        let (generate_tx, generate_rx) = mpsc::channel();
        let (generated_tx, generated_rx) = mpsc::channel();
        let (actor_tx, actor_rx) = mpsc::sync_channel(1);
        let generator_thread = std::thread::spawn(move || -> Result<()> {
            actor_tx.send(actor()?).map_err(|e| e.to_string())?;
            while let Ok(command) = generate_rx.recv_timeout(Duration::from_secs(10)) {
                match command {
                    Generate::Stop => return Ok(()),
                    Generate::Notify { hwnd, kind } => {
                        // Controller has already performed the real SHOW. Notify only afterward.
                        unsafe { NotifyWinEvent(kind, HWND(hwnd as *mut _), OBJID_WINDOW.0, 0) };
                        generated_tx.send(actor()?).map_err(|e| e.to_string())?;
                    }
                }
            }
            Err("owned generator lost its controller".into())
        });
        let generator = receive(&actor_rx, (Instant::now() + READY).min(deadline))?;
        let generator_retained = RetainedActor::retain(generator)?;
        if creator.tid == generator.tid {
            return Err("native generator must differ from creator".into());
        }
        report["creator"] = actor_json(creator);
        report["generator"] = actor_json(generator);
        report["observer"] = actor_json(observer_actor);
        let window_start = Instant::now();
        let window_deadline = (window_start + Duration::from_secs(1)).min(deadline);
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                0,
                0,
                16,
                16,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|e| failure("binding CreateWindowExW", e))?;
        let window = Control(hwnd);
        expected.store(hwnd.0 as usize, Ordering::SeqCst);
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|e| e.to_string())?;
        let nonce = u64::from_le_bytes(random).max(1);
        let registration = Registration {
            hwnd: hwnd.0 as u64,
            nonce,
            generation: 1,
            creator,
            generator,
        };
        window_creator(hwnd, &retained_creator)?;
        report["registration"] = json!({"hwnd": registration.hwnd, "nonce": nonce, "fixture_generation": 1, "creator_api_readback_matches": true});
        requests_tx
            .send(Request::Register(registration))
            .map_err(|e| e.to_string())?;
        response(&responses_rx, window_deadline, "registered")?;
        requests_tx
            .send(Request::Arm(Ticket {
                nonce,
                generation: 1,
                command: 1,
                kind: EVENT_OBJECT_SHOW,
            }))
            .map_err(|e| e.to_string())?;
        response(&responses_rx, window_deadline, "armed")?;
        let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        report["visible_control_readback"] = json!(visible);
        if !visible {
            return Err("native binding visible control was not visible".into());
        }
        generate_tx
            .send(Generate::Notify {
                hwnd: registration.hwnd,
                kind: EVENT_OBJECT_SHOW,
            })
            .map_err(|e| e.to_string())?;
        if receive(&generated_rx, window_deadline)? != generator {
            return Err("actual SHOW generator identity changed".into());
        }
        response(&responses_rx, window_deadline, "show")?;
        requests_tx.send(Request::Pause).map_err(|e| e.to_string())?;
        response(&responses_rx, window_deadline, "paused")?;
        requests_tx
            .send(Request::Arm(Ticket {
                nonce,
                generation: 1,
                command: 2,
                kind: EVENT_OBJECT_DESTROY,
            }))
            .map_err(|e| e.to_string())?;
        response(&responses_rx, window_deadline, "armed")?;
        unsafe { DestroyWindow(hwnd) }.map_err(|e| failure("binding DestroyWindow creator", e))?;
        std::mem::forget(window); // Never revisit this destroyed/recyclable HWND in Drop.
        report["window_lifetime_us"] = json!(window_start.elapsed().as_micros());
        if Instant::now() >= window_deadline {
            return Err("binding visible control exceeded one-second lifetime".into());
        }
        report["creator_destroy_readback"] = gone(hwnd)?;
        requests_tx
            .send(Request::Destroyed { nonce, generation: 1 })
            .map_err(|e| e.to_string())?;
        response(&responses_rx, deadline, "destroyed")?;
        // Only the creator calls DestroyWindow. Its real USER DESTROY notification remains queued
        // until after the creator and observer independently confirm the HWND is already invalid.
        requests_tx.send(Request::Resume).map_err(|e| e.to_string())?;
        response(&responses_rx, deadline, "resumed")?;
        response(&responses_rx, deadline, "destroy")?;
        requests_tx.send(Request::Finish).map_err(|e| e.to_string())?;
        report["observation"] = response(&responses_rx, deadline, "finished")?.ok_or("missing control receipt")?;
        observer_retained.wait_exited(deadline)?;
        observer_thread.join().map_err(|_| "observer thread panicked")??;
        generate_tx.send(Generate::Stop).map_err(|e| e.to_string())?;
        generator_retained.wait_exited(deadline)?;
        generator_thread.join().map_err(|_| "generator thread panicked")??;
        retained_creator.live()?;
        report["cleanup_confirmed"] = json!(true);
        report["complete"] = json!(true);
        report["elapsed_us"] = json!(start.elapsed().as_micros());
        report["cleanup"] = json!({"window_destroyed": true, "generator_retained_exit": true, "observer_retained_exit": true,
            "creator_retained_live_to_receipt": true, "enclosing_process_exit": "waited by outer native fixture launcher"});
        Ok(())
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => report["failure"] = json!(error),
        Err(_) => report["failure"] = json!("owned lifecycle control panicked"),
    }
    if report["observation"].is_null() {
        report["observation"] = progress
            .lock()
            .map(|saved| saved.clone())
            .unwrap_or_else(|_| json!({"failure": "receipt mutex poisoned"}));
    }
    println!("{report}");
}
