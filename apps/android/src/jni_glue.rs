//! The JNI exports of `dev.agentcaller.inbox.Native`, and the Keystore cipher.
//!
//! Every export takes and returns strings (or a `long`/`boolean`), catches
//! panics (an unwinding panic across the JNI boundary aborts the app) and
//! logs nothing: the arguments hold pairing lines and typed answers.
//!
//! The Java side, `KeystoreCipher`, owns the Android Keystore key. Its contract
//! (see `java/dev/agentcaller/inbox/KeystoreCipher.java`): `wrap` and `unwrap`
//! never throw; they return `null` when the Keystore cannot be used, an empty
//! array when the cipher refuses (wrong key, wrong AAD, damaged blob), and the
//! bytes otherwise. `keyState()` (0 usable, 1 permanently unusable, 2 unknown)
//! and `resetKey()` (0 done) back the secure-store reset.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::{Arc, Mutex, OnceLock};

use acd_app_core::wrapped::{self, KeyState, KeyWrap, WrapError};
use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jlong, jstring};
use jni::{JNIEnv, JavaVM};

use crate::bridge::{Bridge, reply};

static BRIDGE: OnceLock<Bridge> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());

const NOT_READY: &str = "the inbox is not started";

/// The Keystore cipher, reached through the Java object the activity passed in.
pub struct JniKeyWrap {
    vm: JavaVM,
    cipher: GlobalRef,
}

impl JniKeyWrap {
    fn call(&self, method: &str, aad: &[u8], data: &[u8]) -> Result<Vec<u8>, WrapError> {
        let mut env = self.vm.attach_current_thread().map_err(|_| WrapError::Unavailable)?;
        let out = env.with_local_frame(8, |env| -> jni::errors::Result<Option<Vec<u8>>> {
            let aad = env.byte_array_from_slice(aad)?;
            let data = env.byte_array_from_slice(data)?;
            let out = env
                .call_method(
                    &self.cipher,
                    method,
                    "([B[B)[B",
                    &[JValue::Object(&aad), JValue::Object(&data)],
                )?
                .l()?;
            if out.is_null() {
                return Ok(None);
            }
            Ok(Some(env.convert_byte_array(JByteArray::from(out))?))
        });
        // The Java side never throws; if something did, drop it and fail closed.
        if env.exception_check().unwrap_or(true) {
            let _ = env.exception_clear();
            return Err(WrapError::Unavailable);
        }
        match out {
            Ok(Some(bytes)) if bytes.is_empty() => Err(WrapError::Failed),
            Ok(Some(bytes)) => Ok(bytes),
            Ok(None) | Err(_) => Err(WrapError::Unavailable),
        }
    }
}

impl JniKeyWrap {
    /// Calls a no-argument `int` method. `None` if anything went wrong.
    fn call_int(&self, method: &str) -> Option<i32> {
        let mut env = self.vm.attach_current_thread().ok()?;
        let out = env.call_method(&self.cipher, method, "()I", &[]).and_then(|v| v.i());
        if env.exception_check().unwrap_or(true) {
            let _ = env.exception_clear();
            return None;
        }
        out.ok()
    }
}

impl KeyWrap for JniKeyWrap {
    fn wrap(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, WrapError> {
        self.call("wrap", aad, plaintext)
    }
    fn unwrap(&self, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, WrapError> {
        self.call("unwrap", aad, blob)
    }
    /// `KeystoreCipher.keyState()`: 0 usable (or not created yet), 1 permanently
    /// unusable, anything else (or any failure to ask) unknown.
    fn key_state(&self) -> KeyState {
        match self.call_int("keyState") {
            Some(0) => KeyState::Usable,
            Some(1) => KeyState::PermanentlyUnusable,
            _ => KeyState::Unknown,
        }
    }
    /// `KeystoreCipher.resetKey()`: deletes the alias, creates a fresh key.
    fn reset_key(&self) -> Result<(), WrapError> {
        match self.call_int("resetKey") {
            Some(0) => Ok(()),
            _ => Err(WrapError::Unavailable),
        }
    }
}

fn arg(env: &mut JNIEnv, value: &JString) -> Result<String, String> {
    env.get_string(value).map(Into::into).map_err(|_| "bad argument".to_owned())
}

fn out(env: &mut JNIEnv, text: String) -> jstring {
    env.new_string(text).map(JString::into_raw).unwrap_or(null_mut())
}

/// Runs `body`, turning a panic into the reply the activity can show.
fn guarded(env: &mut JNIEnv, body: impl FnOnce(&mut JNIEnv) -> String) -> jstring {
    let text = catch_unwind(AssertUnwindSafe(|| body(env)))
        .unwrap_or_else(|_| reply(Err("internal error".into())));
    out(env, text)
}

fn with_bridge(f: impl FnOnce(&Bridge) -> Result<String, String>) -> String {
    reply(BRIDGE.get().ok_or_else(|| NOT_READY.to_owned()).and_then(f))
}

/// Starts the inbox. Returns `null` when it is running (also when it already
/// was), else the reason. `cipher` is the activity's `KeystoreCipher`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_init<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    files_dir: JString<'l>,
    cipher: JObject<'l>,
) -> jstring {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let _once = INIT.lock().unwrap_or_else(|e| e.into_inner());
        if BRIDGE.get().is_some() {
            return Ok(());
        }
        let dir = PathBuf::from(arg(&mut env, &files_dir)?);
        let vm = env.get_java_vm().map_err(|_| "no Java VM".to_owned())?;
        let cipher = env.new_global_ref(&cipher).map_err(|_| "no cipher".to_owned())?;
        wrapped::install(dir.join("secrets"), Arc::new(JniKeyWrap { vm, cipher }));
        let bridge = Bridge::open(&dir, wrapped::installed_or_refusing(), wrapped::installed_reset())?;
        let _ = BRIDGE.set(bridge);
        Ok(())
    }))
    .unwrap_or_else(|_| Err("internal error".into()));
    match result {
        Ok(()) => null_mut(),
        Err(text) => out(&mut env, text),
    }
}

/// The screen model as JSON, or `{"error": ...}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_view<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
) -> jstring {
    guarded(&mut env, |_| match BRIDGE.get() {
        Some(bridge) => bridge.view_json(),
        None => reply(Err(NOT_READY.into())),
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_importPairing<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    text: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| match arg(env, &text) {
        Ok(text) => with_bridge(|b| b.import_pairing(&text)),
        Err(e) => reply(Err(e)),
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_pick<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    card_key: JString<'l>,
    request_id: JString<'l>,
    option_id: JString<'l>,
    revision: jlong,
) -> jstring {
    guarded(&mut env, |env| {
        let args = (|| Ok((arg(env, &card_key)?, arg(env, &request_id)?, arg(env, &option_id)?)))();
        match args {
            Ok((card, request, option)) => {
                with_bridge(|b| b.pick(&card, &request, &option, revision.max(0) as u64))
            }
            Err(e) => reply(Err(e)),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_answerText<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    card_key: JString<'l>,
    request_id: JString<'l>,
    revision: jlong,
    text: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| {
        let args = (|| Ok((arg(env, &card_key)?, arg(env, &request_id)?, arg(env, &text)?)))();
        match args {
            Ok((card, request, text)) => {
                with_bridge(|b| b.answer_text(&card, &request, revision.max(0) as u64, text))
            }
            Err(e) => reply(Err(e)),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_cancel<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    action_id: JString<'l>,
) -> jboolean {
    let done = catch_unwind(AssertUnwindSafe(|| match (arg(&mut env, &action_id), BRIDGE.get()) {
        (Ok(id), Some(bridge)) => bridge.cancel(&id),
        _ => false,
    }))
    .unwrap_or(false);
    jboolean::from(done)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_markSeen<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    card_key: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| match arg(env, &card_key) {
        Ok(key) => with_bridge(|b| b.mark_seen(&key).map(|()| String::new())),
        Err(e) => reply(Err(e)),
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_removeMachine<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    key: JString<'l>,
) -> jstring {
    guarded(&mut env, |env| match arg(env, &key) {
        Ok(key) => with_bridge(|b| b.remove_machine(&key).map(|()| String::new())),
        Err(e) => reply(Err(e)),
    })
}

/// The activity has put `text` on screen. Clears the pending-recovery record
/// if `text` is the current recovery notice. True if it did.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_noticeShown<'l>(
    mut env: JNIEnv<'l>,
    _class: JClass<'l>,
    text: JString<'l>,
) -> jboolean {
    let done = catch_unwind(AssertUnwindSafe(|| match (arg(&mut env, &text), BRIDGE.get()) {
        (Ok(text), Some(bridge)) => bridge.notice_shown(&text),
        _ => false,
    }))
    .unwrap_or(false);
    jboolean::from(done)
}

/// Blocks until the view lists `n` machines. For the host JVM test: the
/// activity polls and never calls it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_agentcaller_inbox_Native_waitForMachines<'l>(
    _env: JNIEnv<'l>,
    _class: JClass<'l>,
    n: jlong,
) -> jboolean {
    let done = catch_unwind(AssertUnwindSafe(|| match BRIDGE.get() {
        Some(bridge) => {
            bridge.wait_for_machines(n.max(0) as usize);
            true
        }
        None => false,
    }))
    .unwrap_or(false);
    jboolean::from(done)
}
