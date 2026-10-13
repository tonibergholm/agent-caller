#ifndef VOICE_BRIDGE_H
#define VOICE_BRIDGE_H
/*
 * The C contract between the app (Rust: app/src-tauri/src/voice/bridge.rs)
 * and the Swift voice bridge. Voice spec §4.2; step-2 plan S1 to S5.
 *
 * Ownership and lifetime:
 * - Swift owns every framework object, Task and AsyncSequence. Rust holds
 *   only generation numbers, never pointers to Swift objects (S3).
 * - vb_init is called once, on the main thread. `on_event` must stay valid for the life of the
 *   process. Swift calls it from one serial queue, never from the audio
 *   thread, and never re-entrantly. It must not block.
 * - vb_event.text is borrowed UTF-8, not NUL-terminated, valid only during
 *   the call. NULL when text_len is 0. The callback copies it.
 * - Every function returns at once. Results arrive as events.
 * - Nothing in an event is an error message: errors are codes (spec §6.3).
 */
#include <stddef.h>
#include <stdint.h>

/* Return codes, and vb_event.code for VB_EV_ERROR and VB_EV_ASSETS. */
#define VB_OK 0
#define VB_ERR_MIC_DENIED 1
#define VB_ERR_MIC_UNAVAILABLE 2
#define VB_ERR_SPEECH_DENIED 3
#define VB_ERR_SPEECH_UNAVAILABLE 4
#define VB_ERR_ASSETS_MISSING 5
#define VB_ERR_BUSY 6 /* the spike probe holds the microphone (S20) */
#define VB_ERR_FAILED 7
#define VB_ERR_INPUT_MONITORING_DENIED 8
#define VB_ERR_TAP_FAILED 9
#define VB_ERR_NOT_INITIALIZED 10
#define VB_ERR_MODEL_UNAVAILABLE 11 /* vb_event.value holds the VB_MA_* reason */
#define VB_ERR_MODEL_TIMEOUT 12
#define VB_ERR_MODEL_FAILED 13      /* any other model error; never its text (spec §6.3) */

/* vb_event.kind, and what each kind sets. */
#define VB_EV_PARTIAL 1u      /* generation; text: the live transcript so far */
#define VB_EV_FINAL 2u        /* generation; text; audio_ms; voiced_ms; value: stop-to-final ms */
#define VB_EV_ERROR 3u        /* generation; code: VB_ERR_* */
#define VB_EV_CAPS 4u         /* code: VB_UA_*; value: streams partials (0/1); value2: sample rate; text: locale id */
#define VB_EV_PERMISSION 5u   /* code: VB_PERM_*; value: VB_PS_* */
#define VB_EV_HOLD 6u         /* value: 1 the hold key went down, 0 up */
#define VB_EV_OTHER_KEY 7u    /* another key went down; never says which (S11) */
#define VB_EV_TAP_DISABLED 8u /* code: 1 timeout, 2 user input; value: 1 if re-enabled */
#define VB_EV_SYSTEM 9u       /* code: VB_SYS_* */
#define VB_EV_ASSETS 10u      /* code: VB_OK or VB_ERR_* */
#define VB_EV_PROBE 11u       /* text: a JSON report with no transcript (Task 1A) */
#define VB_EV_CAPTURE 12u     /* generation; value: 1 the microphone started, 0 it stopped (S18) */
#define VB_EV_OVERFLOW 13u    /* events were dropped since the last delivery (S5) */
#define VB_EV_MODEL 14u       /* generation: the request id; code: VB_OK or VB_ERR_MODEL_*; value: VB_MA_* when unavailable; text: the plan JSON, VB_OK only */
#define VB_EV_MODEL_CAPS 15u  /* code: VB_MA_*; sent only when it changed (plan V16) */
#define VB_EV_SPOKEN 16u  /* generation: the utterance; value: 1 finished, 0 failed (also text that is not UTF-8). Nothing after vb_speech_stop, nor after a capture start (vb_session_start silences speech first, without an event: Rust has stopped it already). */
#define VB_EV_VOICES 17u  /* text: JSON array of {"id","name","language"}, English voices; sent only when it changed */

/* Permissions (vb_event.code for VB_EV_PERMISSION). */
#define VB_PERM_MICROPHONE 1u
#define VB_PERM_SPEECH 2u
#define VB_PERM_INPUT_MONITORING 3u

/* Permission states (vb_event.value for VB_EV_PERMISSION). */
#define VB_PS_NOT_DETERMINED 0u
#define VB_PS_GRANTED 1u
#define VB_PS_DENIED 2u
#define VB_PS_RESTRICTED 3u

/* Why speech is unavailable (vb_event.code for VB_EV_CAPS); 0 = available. */
#define VB_UA_NONE 0u
#define VB_UA_OS_TOO_OLD 1u
#define VB_UA_SDK_TOO_OLD 2u
#define VB_UA_TRANSCRIBER_UNSUPPORTED 3u
#define VB_UA_LOCALE_UNSUPPORTED 4u
#define VB_UA_ASSETS_MISSING 5u

/* Intent model availability (vb_event.code for VB_EV_MODEL_CAPS). */
#define VB_MA_AVAILABLE 0u
#define VB_MA_OS_TOO_OLD 1u
#define VB_MA_SDK_TOO_OLD 2u
#define VB_MA_DEVICE_NOT_ELIGIBLE 3u
#define VB_MA_INTELLIGENCE_OFF 4u
#define VB_MA_NOT_READY 5u
#define VB_MA_UNKNOWN 6u

/* Speaking rates (vb_speech_say). */
#define VB_RATE_SLOW 0u
#define VB_RATE_NORMAL 1u
#define VB_RATE_FAST 2u

/* System events (vb_event.code for VB_EV_SYSTEM). */
#define VB_SYS_SCREEN_LOCKED 1u
#define VB_SYS_WILL_SLEEP 2u
#define VB_SYS_SCREENS_SLEEP 3u
#define VB_SYS_UNLOCKED 4u     /* the screen unlocked or the Mac woke: alerts may speak again (a normal queued event, not the latched lock code) */

typedef struct vb_event {
    uint64_t generation; /* the session's generation; 0 when not a session event */
    uint32_t kind;
    uint32_t code;
    uint32_t value;
    uint32_t value2;
    const uint8_t *text;
    size_t text_len;
    uint64_t audio_ms;
    uint64_t voiced_ms;
} vb_event;

typedef void (*vb_event_fn)(void *ctx, const vb_event *event);

/* Installs the callback and starts the bridge. VB_OK, or VB_ERR_FAILED if
 * on_event is NULL. A second call replaces nothing and returns VB_OK. */
int32_t vb_init(void *ctx, vb_event_fn on_event);

/* Task 1A spike probe; the report arrives as VB_EV_PROBE. */
void vb_probe_run(uint32_t step);

/* Permission state now (VB_PS_*). Any thread. */
uint32_t vb_permission_status(uint32_t permission);

/* Shows the system prompt if the user was never asked. The answer arrives
 * as VB_EV_PERMISSION. */
void vb_permission_request(uint32_t permission);

/* Checks SpeechTranscriber for English on its own task (S4). The answer
 * arrives as VB_EV_CAPS. vb_init runs it once. */
void vb_query_capabilities(void);

/* Downloads the English speech model on its own task, never on the session
 * path (S4, S21). VB_EV_ASSETS, then a new VB_EV_CAPS. A second call while
 * one runs does nothing. */
void vb_install_assets(void);

/* Starts recording `generation` (non-zero). Returns VB_OK, or at once:
 * VB_ERR_NOT_INITIALIZED, VB_ERR_FAILED (generation 0), VB_ERR_MIC_DENIED,
 * VB_ERR_SPEECH_DENIED (only when speech_required is non-zero, plan S14),
 * VB_ERR_SPEECH_UNAVAILABLE, VB_ERR_ASSETS_MISSING, or VB_ERR_BUSY while the
 * spike probe holds the microphone (S20). Every other session
 * is cancelled first, and its capture has stopped before this returns (S4).
 * Capture stops on its own max_ms after it started (S9). Later: VB_EV_CAPTURE
 * when the microphone starts and stops, VB_EV_PARTIAL, then VB_EV_FINAL or
 * VB_EV_ERROR. */
int32_t vb_session_start(uint64_t generation, uint32_t max_ms, uint32_t speech_required);

/* Stops capture before returning, and lets the transcript finish:
 * VB_EV_FINAL or VB_EV_ERROR follows. The finishing session stays
 * cancellable. Ignored for an unknown generation. Never queued (S4). */
void vb_session_stop(uint64_t generation);

/* Stops capture before returning and cancels all of the session's work,
 * finishing or not. Nothing follows but VB_EV_CAPTURE off. Ignored for an
 * unknown generation. Never queued (S4). */
void vb_session_cancel(uint64_t generation);

/* Task 7A: the listen-only hold-key tap. On VB_OK, *out_tap holds the
 * tap's generation; every VB_EV_HOLD, VB_EV_OTHER_KEY and
 * VB_EV_TAP_DISABLED carries it in vb_event.generation (S26). */
int32_t vb_tap_start(uint16_t keycode, uint64_t down_mask, uint64_t *out_tap);
void vb_tap_stop(void);

/*
 * Intent model (plan V1 to V6). Ids are the caller's, non-zero, and separate
 * from session generations. Text arguments are borrowed UTF-8, not
 * NUL-terminated; Swift copies them before returning. Every function returns
 * at once.
 */

/* Plans one step: `instructions` become the session's instructions, `input`
 * its prompt. A fresh model session per call. VB_OK, VB_ERR_NOT_INITIALIZED,
 * or VB_ERR_FAILED (id 0, an id still in flight, or text that is not UTF-8).
 * Later one VB_EV_MODEL for `id`, unless it was cancelled or an overflow
 * (VB_EV_OVERFLOW) dropped it. Swift tries to give up on its own
 * timeout_ms + 1000 ms after the call, but a model that ignores cancellation
 * keeps the call running: Rust's own timeout is the real bound (plan V5). */
int32_t vb_model_plan(uint64_t id, const uint8_t *instructions, size_t instructions_len,
                      const uint8_t *input, size_t input_len, uint32_t timeout_ms);

/* Cancels request `id`; no new VB_EV_MODEL is produced. One already queued
 * may still be delivered after this returns, so the caller drops replies for
 * ids it no longer waits for. Unknown ids are ignored. */
void vb_model_cancel(uint64_t id);

/* Voice off, quit: cancels every request and drops the prewarmed session.
 * Lock and sleep do the same inside Swift. */
void vb_model_cancel_all(void);

/* Prepares one unused session with these instructions, if the model is
 * available. The next vb_model_plan with the same instructions uses it. */
void vb_model_prewarm(const uint8_t *instructions, size_t instructions_len);

/* Reads availability; VB_EV_MODEL_CAPS follows if it changed, or always
 * when `force` is non-zero (Rust forces it after an overflow, which may have
 * dropped the last report). Settings refreshes never force, so they cannot
 * loop (plan V16). */
void vb_model_query(uint32_t force);

/* Speaks `text` as `generation` (non-zero), replacing any speech. `voice` is
 * a voice identifier, or empty for the system's English voice. Text is
 * borrowed and copied before return. VB_EV_SPOKEN follows when it ends. */
void vb_speech_say(uint64_t generation, const uint8_t *text, size_t text_len,
                   const uint8_t *voice, size_t voice_len, uint32_t rate);

/* Stops all speech before it returns (plan V12): the microphone may open
 * next. No VB_EV_SPOKEN follows for the stopped speech. */
void vb_speech_stop(void);

/* Reports the English voices; VB_EV_VOICES follows if they changed, or always when `force` is non-zero (after an overflow). */
void vb_speech_voices(uint32_t force);

#endif
