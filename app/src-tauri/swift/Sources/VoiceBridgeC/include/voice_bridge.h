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

/* System events (vb_event.code for VB_EV_SYSTEM). */
#define VB_SYS_SCREEN_LOCKED 1u
#define VB_SYS_WILL_SLEEP 2u
#define VB_SYS_SCREENS_SLEEP 3u

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

#endif
