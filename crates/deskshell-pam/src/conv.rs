//! The PAM conversation over the worker protocol.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::{Read, Write};

use deskshell_auth::worker::{Outcome, Secret, ToParent, ToWorker, exit};
use zeroize::Zeroize;

use crate::ffi::{
    PAM_BUF_ERR, PAM_CONV_ERR, PAM_ERROR_MSG, PAM_MAX_NUM_MSG, PAM_PROMPT_ECHO_OFF,
    PAM_PROMPT_ECHO_ON, PAM_SUCCESS, PAM_TEXT_INFO, PamConv, PamMessage, PamResponse,
};

pub trait Pam {
    fn authenticate(&mut self, service: &CStr, user: &CStr, conv: &PamConv) -> c_int;
    fn strerror(&self, code: c_int) -> String;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    Aborted,
    Refused,
    Protocol,
}

struct State<R, W> {
    input: R,
    output: W,
    failure: Option<Failure>,
    answered: bool,
}

impl<R: Read, W: Write> State<R, W> {
    fn send(&mut self, message: &ToParent) -> Result<(), Failure> {
        message
            .write_to(&mut self.output)
            .map_err(|_| Failure::Protocol)
    }

    fn prompt(&mut self, echo: bool, text: String) -> Result<Secret, Failure> {
        self.send(&ToParent::Prompt { echo, text })?;
        // A refused answer stays in `reply` and is zeroized when this returns.
        let reply = ToWorker::read_from(&mut self.input);
        match reply {
            Ok(Some(ToWorker::Answer(secret))) if !secret.as_bytes().contains(&0) => {
                self.answered = true;
                Ok(secret)
            }
            Ok(Some(ToWorker::Abort)) => Err(Failure::Aborted),
            _ => Err(Failure::Protocol),
        }
    }

    fn message(&mut self, message: &PamMessage) -> Result<*mut c_char, Failure> {
        let text = if message.msg.is_null() {
            String::new()
        } else {
            // SAFETY: PAM passes NUL terminated message text.
            unsafe { CStr::from_ptr(message.msg) }
                .to_string_lossy()
                .into_owned()
        };
        match message.msg_style {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                let secret = self.prompt(message.msg_style == PAM_PROMPT_ECHO_ON, text)?;
                let copy = malloc_copy(secret.as_bytes());
                if copy.is_null() { Err(Failure::Refused) } else { Ok(copy) }
            }
            PAM_ERROR_MSG => self
                .send(&ToParent::Error(text))
                .map(|()| std::ptr::null_mut()),
            PAM_TEXT_INFO => self
                .send(&ToParent::Info(text))
                .map(|()| std::ptr::null_mut()),
            _ => Err(Failure::Refused),
        }
    }
}

// PAM frees this copy. It is mlocked as a best effort, so the PIN stays out of swap even
// when the worker could not lock all of its memory.
fn malloc_copy(bytes: &[u8]) -> *mut c_char {
    // SAFETY: the allocation is checked and has room for the bytes and the NUL. mlock
    // only changes how the kernel pages the range, which is the live allocation.
    unsafe {
        let copy = libc::malloc(bytes.len() + 1).cast::<u8>();
        if !copy.is_null() {
            libc::mlock(copy.cast(), bytes.len() + 1);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), copy, bytes.len());
            *copy.add(bytes.len()) = 0;
        }
        copy.cast()
    }
}

/// # Safety
///
/// `replies` comes from `calloc(count, ..)` and every non-null `resp` from
/// [`malloc_copy`].
unsafe fn free_replies(replies: *mut PamResponse, count: usize) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        for i in 0..count {
            let resp = (*replies.add(i)).resp;
            if !resp.is_null() {
                std::slice::from_raw_parts_mut(resp.cast::<u8>(), libc::strlen(resp)).zeroize();
                libc::free(resp.cast());
            }
        }
        libc::free(replies.cast());
    }
}

unsafe extern "C" fn conversation<R: Read, W: Write>(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    // SAFETY: `appdata` is the `State` that `run` handed to pam_start. It lives
    // until pam_end, and PAM calls the conversation from one thread.
    let state = unsafe { &mut *appdata.cast::<State<R, W>>() };
    // The first failure is sticky. Later calls fail without any I/O.
    if state.failure.is_some()
        || msg.is_null()
        || resp.is_null()
        || !(1..=PAM_MAX_NUM_MSG).contains(&num_msg)
    {
        return PAM_CONV_ERR;
    }
    let count = num_msg as usize;
    // SAFETY: calloc with a checked count. PAM frees the array on success.
    let replies = unsafe { libc::calloc(count, size_of::<PamResponse>()) }.cast::<PamResponse>();
    if replies.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..count {
        // SAFETY: `msg` holds `num_msg` message pointers.
        let message = unsafe { *msg.add(i) };
        // SAFETY: a non-null message pointer from PAM is valid for this call.
        let result = match unsafe { message.as_ref() } {
            Some(message) => state.message(message),
            None => Err(Failure::Refused),
        };
        match result {
            // SAFETY: `i < count`, inside the calloc'd array.
            Ok(reply) => unsafe { (*replies.add(i)).resp = reply },
            Err(failure) => {
                // SAFETY: `replies` and its responses were allocated above.
                unsafe { free_replies(replies, count) };
                if failure == Failure::Protocol && !state.answered {
                    leave();
                }
                state.failure = Some(failure);
                return PAM_CONV_ERR;
            }
        }
    }
    // SAFETY: `resp` is non-null and PAM owns the array from here.
    unsafe { *resp = replies };
    PAM_SUCCESS
}

// Losing the parent before any answer exits without returning into PAM, so pam_faillock
// never counts an attempt the user did not make. After an answer PAM runs to the end.
fn leave() -> ! {
    // Not `exit`, whose atexit handlers would run on top of a PAM module's stack. The
    // replies are already zeroized and the fds are unbuffered.
    // SAFETY: _exit ends the process at once and has no preconditions.
    unsafe { libc::_exit(c_int::from(exit::PROTOCOL)) }
}

pub fn run<R: Read, W: Write>(
    pam: &mut impl Pam,
    service: &CStr,
    user: &CStr,
    input: R,
    output: W,
) -> u8 {
    let mut state = State { input, output, failure: None, answered: false };
    let conv = PamConv {
        conv: Some(conversation::<R, W>),
        appdata_ptr: std::ptr::from_mut(&mut state).cast(),
    };
    let code = pam.authenticate(service, user, &conv);
    let outcome = match state.failure {
        Some(Failure::Protocol) => return exit::PROTOCOL,
        Some(Failure::Aborted | Failure::Refused) if code == PAM_SUCCESS => {
            Outcome::Failure { code: PAM_CONV_ERR, message: pam.strerror(PAM_CONV_ERR) }
        }
        _ if code == PAM_SUCCESS => Outcome::Success,
        _ => Outcome::Failure { code, message: pam.strerror(code) },
    };
    report(&mut state.output, outcome)
}

pub fn report(output: &mut impl Write, outcome: Outcome) -> u8 {
    let success = outcome.is_success();
    match ToParent::Done(outcome).write_to(output) {
        Err(_) => exit::PROTOCOL,
        Ok(()) if success => exit::SUCCESS,
        Ok(()) => exit::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::ffi::CString;
    use std::io::{self, Cursor};

    use super::*;

    const PAM_AUTH_ERR: c_int = 7;
    const PAM_SYSTEM_ERR: c_int = 4;
    const PAM_BINARY_PROMPT: c_int = 7;
    const PIN_PROMPT: &str = "Please enter the PIN: ";
    const CUE: &str = "Please touch the FIDO authenticator.";

    fn converse(conv: &PamConv, messages: &[(c_int, &str)]) -> Result<Vec<Option<Vec<u8>>>, c_int> {
        let texts: Vec<CString> = messages
            .iter()
            .map(|(_, t)| CString::new(*t).unwrap())
            .collect();
        let structs: Vec<PamMessage> = messages
            .iter()
            .zip(&texts)
            .map(|((style, _), text)| PamMessage { msg_style: *style, msg: text.as_ptr() })
            .collect();
        let mut pointers: Vec<*const PamMessage> = structs
            .iter()
            .map(std::ptr::from_ref)
            .collect();
        let mut resp: *mut PamResponse = std::ptr::null_mut();
        let n = c_int::try_from(messages.len()).unwrap();
        // SAFETY: the arrays outlive the call, and the replies are freed below.
        let code = unsafe {
            (conv.conv.unwrap())(
                n,
                pointers.as_mut_ptr(),
                &raw mut resp,
                conv.appdata_ptr,
            )
        };
        if code != PAM_SUCCESS {
            assert!(
                resp.is_null(),
                "responses handed back with an error"
            );
            return Err(code);
        }
        let mut replies = Vec::new();
        // SAFETY: on success `resp` holds `n` responses allocated with malloc.
        unsafe {
            for i in 0..messages.len() {
                let r = (*resp.add(i)).resp;
                replies.push((!r.is_null()).then(|| CStr::from_ptr(r).to_bytes().to_vec()));
                libc::free(r.cast());
            }
            libc::free(resp.cast());
        }
        Ok(replies)
    }

    struct FakeU2f {
        pin: &'static [u8],
        ignore_conv_errors: bool,
        extra_prompt: bool,
    }

    impl FakeU2f {
        fn new() -> Self {
            Self { pin: b"123456", ignore_conv_errors: false, extra_prompt: false }
        }
    }

    impl Pam for FakeU2f {
        fn authenticate(&mut self, service: &CStr, user: &CStr, conv: &PamConv) -> c_int {
            assert_eq!(service.to_bytes(), b"deskshell-test");
            assert_eq!(user.to_bytes(), b"tester");
            let answer = match converse(conv, &[(PAM_PROMPT_ECHO_OFF, PIN_PROMPT)]) {
                Ok(replies) => replies[0].clone().unwrap_or_default(),
                Err(_) if self.ignore_conv_errors => return PAM_SUCCESS,
                Err(_) => return PAM_AUTH_ERR,
            };
            let _ = converse(conv, &[(PAM_TEXT_INFO, CUE)]);
            if self.extra_prompt
                && let Err(code) = converse(conv, &[(PAM_PROMPT_ECHO_ON, "again")])
            {
                assert_eq!(code, PAM_CONV_ERR);
                return PAM_AUTH_ERR;
            }
            if answer == self.pin { PAM_SUCCESS } else { PAM_AUTH_ERR }
        }

        fn strerror(&self, code: c_int) -> String {
            match code {
                PAM_SUCCESS => "Success",
                PAM_SYSTEM_ERR => "System error",
                PAM_AUTH_ERR => "Authentication failure",
                PAM_CONV_ERR => "Conversation error",
                _ => "Unknown PAM error",
            }
            .into()
        }
    }

    struct Script<F>(F);

    impl<F: FnMut(&PamConv) -> c_int> Pam for Script<F> {
        fn authenticate(&mut self, _: &CStr, _: &CStr, conv: &PamConv) -> c_int {
            (self.0)(conv)
        }

        fn strerror(&self, code: c_int) -> String {
            format!("error {code}")
        }
    }

    fn parent(frames: &[ToWorker]) -> Cursor<Vec<u8>> {
        Cursor::new(
            frames
                .iter()
                .flat_map(|f| f.encode().unwrap().to_vec())
                .collect(),
        )
    }

    fn answer(pin: &[u8]) -> ToWorker {
        ToWorker::Answer(Secret::new(pin))
    }

    fn frames(bytes: &[u8]) -> Vec<ToParent> {
        let mut cursor = bytes;
        let mut out = Vec::new();
        while let Some(frame) = ToParent::read_from(&mut cursor).unwrap() {
            out.push(frame);
        }
        out
    }

    fn drive(pam: &mut impl Pam, input: Cursor<Vec<u8>>) -> (u8, Vec<ToParent>) {
        let service = CString::new("deskshell-test").unwrap();
        let user = CString::new("tester").unwrap();
        let mut output = Vec::new();
        let status = run(pam, &service, &user, input, &mut output);
        (status, frames(&output))
    }

    fn pin_prompt() -> ToParent {
        ToParent::Prompt { echo: false, text: PIN_PROMPT.into() }
    }

    fn failure(code: c_int, message: &str) -> ToParent {
        ToParent::Done(Outcome::Failure { code, message: message.into() })
    }

    #[test]
    fn right_pin() {
        let (status, out) = drive(&mut FakeU2f::new(), parent(&[answer(b"123456")]));
        assert_eq!(status, exit::SUCCESS);
        assert_eq!(
            out,
            [pin_prompt(), ToParent::Info(CUE.into()), ToParent::Done(Outcome::Success)]
        );
    }

    #[test]
    fn wrong_pin() {
        let (status, out) = drive(&mut FakeU2f::new(), parent(&[answer(b"000000")]));
        assert_eq!(status, exit::FAILURE);
        assert_eq!(
            out,
            [
                pin_prompt(),
                ToParent::Info(CUE.into()),
                failure(PAM_AUTH_ERR, "Authentication failure")
            ]
        );
    }

    #[test]
    fn abort_is_a_reported_failure() {
        let (status, out) = drive(&mut FakeU2f::new(), parent(&[ToWorker::Abort]));
        assert_eq!(status, exit::FAILURE);
        assert_eq!(
            out,
            [pin_prompt(), failure(PAM_AUTH_ERR, "Authentication failure")]
        );
    }

    #[test]
    fn abort_never_becomes_success() {
        let mut pam = FakeU2f { ignore_conv_errors: true, ..FakeU2f::new() };
        let (status, out) = drive(&mut pam, parent(&[ToWorker::Abort]));
        assert_eq!(status, exit::FAILURE);
        assert_eq!(
            out,
            [pin_prompt(), failure(PAM_CONV_ERR, "Conversation error")]
        );
    }

    fn broken_input(case: &str, first: &[ToWorker]) -> Cursor<Vec<u8>> {
        let mut input = parent(first);
        let bytes = input.get_mut();
        match case {
            "eof" => {}
            "garbage" => bytes.extend_from_slice(&[b'Q', 0, 0, 0, 0]),
            "oversized" => {
                bytes.push(b'A');
                bytes.extend_from_slice(&5000u32.to_le_bytes());
            }
            "truncated" => bytes.extend_from_slice(&[b'A', 6, 0, 0, 0, b'1']),
            "nul" => bytes.extend_from_slice(&answer(b"12\x0034").encode().unwrap()),
            _ => panic!("no case {case}"),
        }
        input
    }

    const LOSSES: [&str; 5] = ["eof", "garbage", "oversized", "truncated", "nul"];

    #[test]
    fn losing_the_parent_after_an_answer_runs_pam_to_the_end() {
        for case in LOSSES {
            let mut pam = FakeU2f { extra_prompt: true, ..FakeU2f::new() };
            let (status, out) = drive(&mut pam, broken_input(case, &[answer(b"123456")]));
            assert_eq!(status, exit::PROTOCOL, "{case}");
            assert_eq!(
                out,
                [
                    pin_prompt(),
                    ToParent::Info(CUE.into()),
                    ToParent::Prompt { echo: true, text: "again".into() },
                ],
                "{case}"
            );
        }
    }

    const CHILD: &str = "DESKSHELL_PAM_TEST_CHILD";
    const RETURNED: &str = "returned into PAM";

    // A copy of this test binary, because the worker ends the process.
    fn in_child(case: &str) -> (Option<i32>, String) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "conv::tests::child", "--nocapture", "--test-threads=1"])
            .env(CHILD, case)
            .output()
            .unwrap();
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    // Runs only as the child of `in_child`. In a plain test run it does nothing.
    #[test]
    fn child() {
        let Ok(case) = std::env::var(CHILD) else {
            return;
        };
        let service = CString::new("deskshell-test").unwrap();
        let user = CString::new("tester").unwrap();
        let mut pam = Script(|conv: &PamConv| {
            let first =
                if case == "info-write-fails" { PAM_TEXT_INFO } else { PAM_PROMPT_ECHO_OFF };
            let result = converse(conv, &[(first, PIN_PROMPT)]);
            eprintln!("{RETURNED}: {result:?}");
            PAM_AUTH_ERR
        });
        let status = if case.ends_with("write-fails") {
            run(&mut pam, &service, &user, parent(&[]), Closed)
        } else {
            run(
                &mut pam,
                &service,
                &user,
                broken_input(&case, &[]),
                Vec::new(),
            )
        };
        eprintln!("run returned {status}");
    }

    #[test]
    fn losing_the_parent_before_an_answer_exits_at_once() {
        for case in LOSSES
            .into_iter()
            .chain(["prompt-write-fails", "info-write-fails"])
        {
            let (status, stderr) = in_child(case);
            assert_eq!(status, Some(3), "{case}: {stderr}");
            assert!(!stderr.contains(RETURNED), "{case}: {stderr}");
            assert!(
                !stderr.contains("run returned"),
                "{case}: {stderr}"
            );
        }
    }

    #[test]
    fn failure_is_sticky() {
        let mut pam = FakeU2f { extra_prompt: true, ..FakeU2f::new() };
        let (status, out) = drive(
            &mut pam,
            parent(&[answer(b"123456"), ToWorker::Abort]),
        );
        assert_eq!(status, exit::FAILURE);
        assert_eq!(
            out,
            [
                pin_prompt(),
                ToParent::Info(CUE.into()),
                ToParent::Prompt { echo: true, text: "again".into() },
                failure(PAM_AUTH_ERR, "Authentication failure"),
            ]
        );

        let mut calls = 0;
        let mut pam = Script(|conv: &PamConv| {
            calls += 1;
            assert_eq!(
                converse(conv, &[(PAM_PROMPT_ECHO_OFF, "a")]),
                Err(PAM_CONV_ERR)
            );
            assert_eq!(
                converse(conv, &[(PAM_TEXT_INFO, "after")]),
                Err(PAM_CONV_ERR)
            );
            assert_eq!(
                converse(conv, &[(PAM_PROMPT_ECHO_OFF, "b")]),
                Err(PAM_CONV_ERR)
            );
            PAM_SUCCESS
        });
        let (status, out) = drive(&mut pam, parent(&[ToWorker::Abort, answer(b"1")]));
        assert_eq!(calls, 1);
        assert_eq!(status, exit::FAILURE);
        assert_eq!(
            out,
            [ToParent::Prompt { echo: false, text: "a".into() }, failure(PAM_CONV_ERR, "error 19")]
        );
    }

    #[test]
    fn unsupported_style_fails_the_conversation() {
        let mut pam = Script(|conv: &PamConv| {
            assert_eq!(
                converse(conv, &[(PAM_BINARY_PROMPT, "bin")]),
                Err(PAM_CONV_ERR)
            );
            PAM_SUCCESS
        });
        let (status, out) = drive(&mut pam, parent(&[]));
        assert_eq!(status, exit::FAILURE);
        assert_eq!(out, [failure(PAM_CONV_ERR, "error 19")]);
    }

    #[test]
    fn several_messages_in_one_call() {
        let mut pam = Script(|conv: &PamConv| {
            let replies = converse(
                conv,
                &[
                    (PAM_TEXT_INFO, "info"),
                    (PAM_PROMPT_ECHO_OFF, "pin"),
                    (PAM_ERROR_MSG, "careful"),
                    (PAM_PROMPT_ECHO_ON, "name"),
                ],
            )
            .unwrap();
            assert_eq!(
                replies,
                [None, Some(b"1234".to_vec()), None, Some(b"me".to_vec())]
            );
            PAM_SUCCESS
        });
        let (status, out) = drive(
            &mut pam,
            parent(&[answer(b"1234"), answer(b"me")]),
        );
        assert_eq!(status, exit::SUCCESS);
        assert_eq!(
            out,
            [
                ToParent::Info("info".into()),
                ToParent::Prompt { echo: false, text: "pin".into() },
                ToParent::Error("careful".into()),
                ToParent::Prompt { echo: true, text: "name".into() },
                ToParent::Done(Outcome::Success),
            ]
        );
    }

    #[test]
    fn failed_call_frees_earlier_replies() {
        let mut pam = Script(|conv: &PamConv| {
            let result = converse(
                conv,
                &[(PAM_PROMPT_ECHO_OFF, "pin"), (PAM_PROMPT_ECHO_OFF, "two")],
            );
            assert_eq!(result, Err(PAM_CONV_ERR));
            PAM_AUTH_ERR
        });
        let (status, out) = drive(
            &mut pam,
            parent(&[answer(b"1234"), ToWorker::Abort]),
        );
        assert_eq!(status, exit::FAILURE);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn bad_arguments_are_conv_errors() {
        let mut pam = Script(|conv: &PamConv| {
            let mut resp: *mut PamResponse = std::ptr::null_mut();
            let call = conv.conv.unwrap();
            // SAFETY: the callback checks these arguments before touching them.
            unsafe {
                assert_eq!(
                    call(
                        0,
                        std::ptr::null_mut(),
                        &raw mut resp,
                        conv.appdata_ptr
                    ),
                    PAM_CONV_ERR
                );
                assert_eq!(
                    call(
                        1,
                        std::ptr::null_mut(),
                        &raw mut resp,
                        conv.appdata_ptr
                    ),
                    PAM_CONV_ERR
                );
                assert_eq!(
                    call(
                        33,
                        std::ptr::null_mut(),
                        &raw mut resp,
                        conv.appdata_ptr
                    ),
                    PAM_CONV_ERR
                );
                let mut null_message: *const PamMessage = std::ptr::null();
                assert_eq!(
                    call(
                        1,
                        &raw mut null_message,
                        &raw mut resp,
                        conv.appdata_ptr
                    ),
                    PAM_CONV_ERR
                );
            }
            assert!(resp.is_null());
            PAM_AUTH_ERR
        });
        let (status, out) = drive(&mut pam, parent(&[]));
        assert_eq!(status, exit::FAILURE);
        assert_eq!(out, [failure(PAM_AUTH_ERR, "error 7")]);
    }

    #[test]
    fn pam_start_failure_is_reported() {
        let mut pam = Script(|_: &PamConv| PAM_SYSTEM_ERR);
        let (status, out) = drive(&mut pam, parent(&[]));
        assert_eq!(status, exit::FAILURE);
        assert_eq!(out, [failure(PAM_SYSTEM_ERR, "error 4")]);
    }

    #[test]
    fn long_prompt_is_clipped() {
        let long = "x".repeat(6000);
        let mut pam = Script(|conv: &PamConv| {
            converse(conv, &[(PAM_TEXT_INFO, &long)]).unwrap();
            PAM_SUCCESS
        });
        let (status, out) = drive(&mut pam, parent(&[]));
        assert_eq!(status, exit::SUCCESS);
        assert_matches!(&out[0], ToParent::Info(text) if text.len() == 4096);
    }

    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn closed_output_exits_3() {
        let service = CString::new("deskshell-test").unwrap();
        let user = CString::new("tester").unwrap();
        let mut pam = Script(|_: &PamConv| PAM_SUCCESS);
        assert_eq!(
            run(&mut pam, &service, &user, parent(&[]), Closed),
            exit::PROTOCOL
        );
    }

    #[test]
    fn report_statuses() {
        let mut out = Vec::new();
        assert_eq!(report(&mut out, Outcome::Success), exit::SUCCESS);
        let fail = Outcome::Failure { code: 10, message: "User not known".into() };
        assert_eq!(report(&mut out, fail.clone()), exit::FAILURE);
        assert_eq!(
            report(&mut Closed, Outcome::Success),
            exit::PROTOCOL
        );
        assert_eq!(
            frames(&out),
            [ToParent::Done(Outcome::Success), ToParent::Done(fail)]
        );
    }
}
