package dev.agentcaller.inbox;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.graphics.Color;
import android.graphics.Typeface;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.Editable;
import android.text.InputType;
import android.text.TextWatcher;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import org.json.JSONArray;
import org.json.JSONObject;

import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The foreground inbox. All decisions (which options exist, which are enabled,
 * whether a card is read-only) are made in Rust and arrive in the view JSON;
 * this class draws rows and sends back the ids it was given. It refreshes
 * once a second while it is on screen and does nothing in the background.
 * Every call into the native library, the Keystore access behind it included,
 * runs on one worker thread, never on the UI thread, so a slow provider cannot
 * stall input or trigger an ANR; a refresh that is still running is not
 * started twice. (Failed secret reads are remembered for a few seconds in
 * Rust, so a broken provider is not asked again every tick.)
 *
 * Written without lambdas and newer language features: the sandbox build
 * converts to dex with dx, which does not desugar.
 */
public class MainActivity extends Activity {
    private static final long REFRESH_MS = 1000;

    private final Handler ui = new Handler(Looper.getMainLooper());
    private final Drafts drafts = new Drafts();
    private LinearLayout cards;
    private LinearLayout machines;
    private TextView status;
    private TextView notice;
    private Button noticeDismiss;
    private EditText pairingText;
    private TextView pairingResult;
    private String lastView = "";
    /** The option waiting for its second tap, bound to the request it was tapped on. */
    private static final long CONFIRM_MS = 5000; // as app/src/lib/keys.ts
    private String armedKey = null; // cardKey|optionId|requestId|revision
    private long armedAt = 0;
    private String startError = null;
    private boolean resumed = false;
    /** Serial: init runs first, then every view and action, in order. */
    private final ExecutorService worker = Executors.newSingleThreadExecutor();
    private boolean refreshing = false; // UI thread only
    private boolean refreshAgain = false; // UI thread only
    private boolean pairing = false; // UI thread only: one pairing (and recovery) at a time
    private final NoticeTracker noticeTracker = new NoticeTracker(); // UI thread only

    private final Runnable tick = new Runnable() {
        @Override
        public void run() {
            if (!resumed) return;
            refresh();
            ui.postDelayed(this, REFRESH_MS);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // Pairing lines and requests are not for screenshots or the recents screen.
        getWindow().setFlags(WindowManager.LayoutParams.FLAG_SECURE,
                WindowManager.LayoutParams.FLAG_SECURE);
        buildLayout();
        final String filesDir = getFilesDir().getAbsolutePath();
        worker.execute(new Runnable() {
            @Override
            public void run() {
                String error;
                try {
                    error = Native.init(filesDir, new KeystoreCipher());
                } catch (Throwable t) {
                    error = "The native library could not be loaded.";
                }
                final String result = error;
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        startError = result;
                        lastView = "";
                        refresh();
                    }
                });
            }
        });
    }

    @Override
    protected void onDestroy() {
        worker.shutdown();
        super.onDestroy();
    }

    @Override
    protected void onResume() {
        super.onResume();
        resumed = true;
        noticeTracker.onResume();
        ui.post(tick);
    }

    @Override
    protected void onPause() {
        resumed = false;
        noticeTracker.onPause();
        ui.removeCallbacks(tick);
        super.onPause();
    }

    // ---- layout --------------------------------------------------------

    private void buildLayout() {
        ScrollView scroll = new ScrollView(this);
        LinearLayout root = column();
        root.setPadding(dp(16), dp(16), dp(16), dp(32));
        scroll.addView(root);

        TextView title = text("Agent Caller", 22, true);
        root.addView(title);
        status = text("Starting…", 14, false);
        root.addView(status);
        notice = text("", 13, true);
        notice.setTextColor(Color.rgb(200, 120, 0));
        notice.setVisibility(View.GONE);
        root.addView(notice);
        noticeDismiss = button("Dismiss", new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                dismissNotice();
            }
        });
        noticeDismiss.setVisibility(View.GONE);
        root.addView(noticeDismiss);

        cards = column();
        root.addView(cards);

        root.addView(text("Machines", 16, true));
        machines = column();
        root.addView(machines);

        root.addView(text("Pair a machine", 16, true));
        root.addView(text("On the machine, run `acd pair` and paste the acd:// line here. "
                + "Only Tailscale addresses are accepted.", 13, false));
        pairingText = new EditText(this);
        pairingText.setHint("acd://...");
        pairingText.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE
                | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        pairingText.setMinLines(2);
        root.addView(pairingText);
        LinearLayout buttons = new LinearLayout(this);
        buttons.addView(button("Paste", new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pasteFromClipboard();
            }
        }));
        buttons.addView(button("Pair", new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                pair();
            }
        }));
        root.addView(buttons);
        pairingResult = text("", 13, false);
        root.addView(pairingResult);
        setContentView(scroll);
    }

    private LinearLayout column() {
        LinearLayout l = new LinearLayout(this);
        l.setOrientation(LinearLayout.VERTICAL);
        return l;
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }

    private TextView text(String s, int sp, boolean bold) {
        TextView t = new TextView(this);
        t.setText(s);
        t.setTextSize(sp);
        if (bold) t.setTypeface(Typeface.DEFAULT_BOLD);
        t.setPadding(0, dp(bold ? 12 : 2), 0, dp(2));
        return t;
    }

    private Button button(String label, View.OnClickListener l) {
        Button b = new Button(this);
        b.setText(label);
        b.setMinHeight(dp(48));
        b.setOnClickListener(l);
        return b;
    }

    // ---- refresh -------------------------------------------------------

    /** UI thread. Asks the worker for the view and draws it when it arrives. */
    private void refresh() {
        if (startError != null) {
            status.setText("Cannot start: " + startError);
            return;
        }
        if (refreshing) {
            refreshAgain = true;
            return;
        }
        refreshing = true;
        try {
            worker.execute(new Runnable() {
                @Override
                public void run() {
                    final String json = Native.view();
                    ui.post(new Runnable() {
                        @Override
                        public void run() {
                            refreshing = false;
                            draw(json);
                            if (refreshAgain) {
                                refreshAgain = false;
                                refresh();
                            }
                        }
                    });
                }
            });
        } catch (java.util.concurrent.RejectedExecutionException e) {
            refreshing = false; // shut down: the activity is going away
        }
    }

    private void draw(String json) {
        if (json == null || json.equals(lastView)) return;
        lastView = json;
        try {
            render(new JSONObject(json));
        } catch (Exception e) {
            status.setText("The screen could not be read.");
        }
    }

    private void render(JSONObject view) throws Exception {
        if (view.has("error")) {
            status.setText(view.getString("error"));
            return;
        }
        String note = view.isNull("notice") ? null : view.optString("notice", null);
        // Drawing is not showing: a view that arrives after onPause() is drawn
        // into a screen nobody sees. Nothing is acknowledged here; only a tap
        // on Dismiss, while resumed, is (see NoticeTracker).
        note = noticeTracker.update(note);
        notice.setText(note == null ? "" : note);
        notice.setVisibility(note == null ? View.GONE : View.VISIBLE);
        noticeDismiss.setVisibility(note == null ? View.GONE : View.VISIBLE);
        int needs = view.getInt("needs_you");
        JSONArray machineRows = view.getJSONArray("machines");
        status.setText(machineRows.length() == 0
                ? "No machine paired yet."
                : (needs == 0 ? "Nothing needs you." : needs + " need you."));
        machines.removeAllViews();
        for (int i = 0; i < machineRows.length(); i++) {
            final JSONObject m = machineRows.getJSONObject(i);
            LinearLayout row = new LinearLayout(this);
            String line = m.getString("name") + " · " + m.getString("status");
            TextView t = text(line, 14, false);
            t.setLayoutParams(new LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
            row.addView(t);
            row.addView(button("Remove", new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    removeMachine(m.optString("key"));
                }
            }));
            machines.addView(row);
            if (!m.isNull("problem")) {
                machines.addView(text(m.getString("problem"), 12, false));
            }
        }
        cards.removeAllViews();
        JSONArray cardRows = view.getJSONArray("cards");
        for (int i = 0; i < cardRows.length(); i++) {
            cards.addView(card(cardRows.getJSONObject(i)));
        }
        // A question answered elsewhere, or an agent that is gone, leaves no draft.
        drafts.prune(Drafts.pendingKeys(cardRows));
    }

    private View card(final JSONObject c) throws Exception {
        final String key = c.getString("key");
        LinearLayout box = column();
        box.setPadding(dp(12), dp(12), dp(12), dp(12));
        box.setBackgroundColor(Color.argb(24, 128, 128, 128));
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.setMargins(0, dp(8), 0, dp(8));
        box.setLayoutParams(lp);

        box.addView(text(c.getString("title") + (c.optBoolean("busy") ? " …" : ""), 16, true));
        box.addView(text(c.getString("subtitle"), 12, false));
        if (!c.isNull("last_message")) box.addView(text(c.getString("last_message"), 13, false));
        if (!c.isNull("offline_note")) box.addView(text(c.getString("offline_note"), 13, false));

        JSONArray detail = c.getJSONArray("detail");
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < detail.length(); i++) {
            if (i > 0) sb.append('\n');
            sb.append(detail.getString(i));
        }
        if (sb.length() > 0) {
            TextView d = text(sb.toString(), 13, false);
            d.setTypeface(Typeface.MONOSPACE);
            d.setTextIsSelectable(true);
            box.addView(d);
        }

        final String requestId = c.isNull("request_id") ? null : c.getString("request_id");
        final long revision = c.getLong("revision");
        JSONArray options = c.getJSONArray("options");
        for (int i = 0; i < options.length(); i++) {
            final JSONObject o = options.getJSONObject(i);
            final String optionId = o.getString("id");
            final String armKey = key + "|" + optionId + "|" + requestId + "|" + revision;
            // The model's text: the label, then the description on its own line
            // (what tells "Yes" from "Yes, always"). Drawn whole: the button
            // wraps and is never ellipsized or limited to one line.
            String label = o.getString("text");
            if (isArmed(armKey)) label = "Tap again to " + label;
            Button b = button(label, null);
            b.setSingleLine(false);
            b.setMaxLines(Integer.MAX_VALUE);
            b.setEllipsize(null);
            b.setAllCaps(false);
            b.setEnabled(o.getBoolean("enabled") && requestId != null && !c.optBoolean("busy"));
            final boolean confirm = o.getBoolean("confirm");
            b.setOnClickListener(new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    // The second tap counts only for the same request and revision,
                    // within CONFIRM_MS: the same rule as the Mac panel.
                    if (confirm && !isArmed(armKey)) {
                        armedKey = armKey;
                        armedAt = android.os.SystemClock.elapsedRealtime();
                        lastView = "";
                        refresh();
                        ui.postDelayed(new Runnable() {
                            @Override
                            public void run() {
                                lastView = "";
                                refresh();
                            }
                        }, CONFIRM_MS + 100);
                        return;
                    }
                    armedKey = null;
                    pick(key, requestId, optionId, revision);
                }
            });
            box.addView(b);
            if (!o.getBoolean("enabled")) box.addView(text("Not supported here", 11, false));
        }

        if (c.optBoolean("free_text") && requestId != null) {
            final EditText answer = new EditText(this);
            answer.setHint("Type an answer");
            String prior = drafts.get(key, requestId);
            if (prior != null) answer.setText(prior);
            answer.addTextChangedListener(new TextWatcher() {
                @Override public void beforeTextChanged(CharSequence s, int a, int b, int n) {}
                @Override public void onTextChanged(CharSequence s, int a, int b, int n) {}
                @Override public void afterTextChanged(Editable e) {
                    drafts.put(key, requestId, e.toString());
                }
            });
            box.addView(answer);
            box.addView(button("Send answer", new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    answerText(key, requestId, revision, answer.getText().toString());
                }
            }));
        }

        if (!c.isNull("read_only")) {
            TextView ro = text(c.getString("read_only"), 13, true);
            box.addView(ro);
        }
        if (c.optBoolean("can_mark_seen")) {
            box.addView(button("Mark seen", new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    runReply(new Job() {
                        @Override
                        public String run() {
                            return Native.markSeen(key);
                        }
                    });
                }
            }));
        }
        if (!c.isNull("notice")) {
            TextView n = text(c.getString("notice"), 13, false);
            if (c.optBoolean("notice_warning")) n.setTextColor(Color.rgb(200, 120, 0));
            box.addView(n);
        }
        if (!c.isNull("undo")) {
            final JSONObject u = c.getJSONObject("undo");
            box.addView(text("Sending: " + u.getString("label"), 13, true));
            box.addView(button("Undo", new View.OnClickListener() {
                @Override
                public void onClick(View v) {
                    final String id = u.optString("id");
                    runReply(new Job() {
                        @Override
                        public String run() {
                            Native.cancel(id);
                            return "{\"ok\":\"\"}";
                        }
                    });
                }
            }));
        }
        return box;
    }

    private boolean isArmed(String armKey) {
        return armKey.equals(armedKey)
                && android.os.SystemClock.elapsedRealtime() - armedAt <= CONFIRM_MS;
    }

    // ---- actions -------------------------------------------------------

    private interface Job {
        /** Runs off the main thread; returns the JSON reply. */
        String run();
    }

    private void runReply(final Job job) {
        runReply(job, null);
    }

    /** As above; {@code onOk} runs on the main thread after an "ok" reply. */
    private void runReply(final Job job, final Runnable onOk) {
        new Thread(new Runnable() {
            @Override
            public void run() {
                final String reply = job.run();
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        showReply(reply);
                        if (onOk != null && reply.contains("\"ok\"")) onOk.run();
                        lastView = "";
                        refresh();
                    }
                });
            }
        }).start();
    }

    private void showReply(String reply) {
        try {
            JSONObject r = new JSONObject(reply);
            if (r.has("error")) status.setText(r.getString("error"));
        } catch (Exception e) {
            status.setText("Unexpected reply.");
        }
    }

    private void pick(final String key, final String requestId, final String optionId, final long revision) {
        runReply(new Job() {
            @Override
            public String run() {
                return Native.pick(key, requestId, optionId, revision);
            }
        });
    }

    private void answerText(final String key, final String requestId, final long revision, final String text) {
        runReply(new Job() {
            @Override
            public String run() {
                return Native.answerText(key, requestId, revision, text);
            }
        }, new Runnable() {
            @Override
            public void run() {
                drafts.remove(key, requestId); // only touched on the main thread
            }
        });
    }

    private void removeMachine(final String key) {
        runReply(new Job() {
            @Override
            public String run() {
                return Native.removeMachine(key);
            }
        });
    }

    private void pasteFromClipboard() {
        ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        if (cm == null || !cm.hasPrimaryClip() || cm.getPrimaryClip().getItemCount() == 0) {
            pairingResult.setText("The clipboard is empty.");
            return;
        }
        CharSequence t = cm.getPrimaryClip().getItemAt(0).coerceToText(this);
        pairingText.setText(t == null ? "" : t.toString());
    }

    /**
     * The user tapped Dismiss. Only now does the native side delete the
     * pending-recovery record, off the main thread (it deletes a file). A tap
     * that does not count (not resumed) does nothing and keeps the record.
     */
    private void dismissNotice() {
        final String text = noticeTracker.dismiss();
        if (text == null) return;
        notice.setVisibility(View.GONE);
        noticeDismiss.setVisibility(View.GONE);
        new Thread(new Runnable() {
            @Override
            public void run() {
                Native.noticeShown(text);
            }
        }).start();
    }

    private void pair() {
        // A second tap while one pairing runs would start a second recovery of
        // the Keystore key. The native side serialises them as well; this
        // keeps the second tap from queueing a thread at all.
        if (pairing) return;
        pairing = true;
        final String line = pairingText.getText().toString();
        pairingResult.setText("Pairing…");
        new Thread(new Runnable() {
            @Override
            public void run() {
                final String reply = Native.importPairing(line);
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        pairing = false;
                        try {
                            JSONObject r = new JSONObject(reply);
                            if (r.has("ok")) {
                                pairingResult.setText("Paired " + r.getString("ok") + ".");
                                // The line holds the pairing secret: drop it from the
                                // field and from the clipboard if it is still there.
                                pairingText.setText("");
                                clearClipboardIf(line);
                            } else {
                                pairingResult.setText(r.optString("error", "Pairing failed."));
                            }
                        } catch (Exception e) {
                            pairingResult.setText("Pairing failed.");
                        }
                        lastView = "";
                        refresh();
                    }
                });
            }
        }).start();
    }

    private void clearClipboardIf(String line) {
        ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        if (cm == null || !cm.hasPrimaryClip() || cm.getPrimaryClip().getItemCount() == 0) return;
        CharSequence now = cm.getPrimaryClip().getItemAt(0).coerceToText(this);
        if (now != null && now.toString().trim().equals(line.trim())) {
            cm.setPrimaryClip(ClipData.newPlainText("", ""));
        }
    }
}
