package org.marathonos.deckard.bridge;

import android.accessibilityservice.AccessibilityService;
import android.content.ComponentName;
import android.content.Context;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.Settings;
import android.util.Log;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The front app's screen, for Pilot (protocol/bridge.md, Keel Actions). Bound
 * only on demand: enabled for a request, disabled two minutes after the last.
 */
public class ScreenReader extends AccessibilityService {
    private static final int MAX_NODES = 300;
    private static final int MAX_VISITS = 3000;
    private static final long IDLE_MS = 120_000;
    private static final long BIND_WAIT_MS = 4000;

    private static final ExecutorService sWorker = Executors.newSingleThreadExecutor();
    private static final Handler sMain = new Handler(Looper.getMainLooper());
    private static volatile ScreenReader sInstance;
    private static volatile Context sContext;
    private static final Runnable sDisable = () -> setEnabled(sContext, false);
    // The last read's nodes, by index; only the worker touches these.
    private static final List<AccessibilityNodeInfo> sNodes = new ArrayList<>();
    private static String sPkg = "";
    private static long sSnap;

    @Override
    protected void onServiceConnected() {
        sInstance = this;
    }

    @Override
    public boolean onUnbind(android.content.Intent intent) {
        sInstance = null;
        return super.onUnbind(intent);
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
    }

    @Override
    public void onInterrupt() {
    }

    static void read(Context context, JSONObject m, Link link) {
        sWorker.execute(() -> link.send(readNow(context, m)));
    }

    static void act(Context context, JSONObject m, Link link) {
        sWorker.execute(() -> link.send(actNow(context, m)));
    }

    private static ComponentName component(Context context) {
        return new ComponentName(context, ScreenReader.class);
    }

    private static synchronized void setEnabled(Context context, boolean on) {
        if (context == null)
            return;
        String me = component(context).flattenToString();
        String value = Settings.Secure.getString(context.getContentResolver(),
                Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES);
        StringBuilder out = new StringBuilder();
        boolean present = false;
        if (value != null) {
            for (String s : value.split(":")) {
                if (s.isEmpty())
                    continue;
                if (s.equals(me)) {
                    present = true;
                    if (!on)
                        continue;
                }
                out.append(out.length() > 0 ? ":" : "").append(s);
            }
        }
        if (on && !present)
            out.append(out.length() > 0 ? ":" : "").append(me);
        if (on == present)
            return;
        Settings.Secure.putString(context.getContentResolver(),
                Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES, out.toString());
        Settings.Secure.putInt(context.getContentResolver(), Settings.Secure.ACCESSIBILITY_ENABLED, 1);
        Log.i(Link.TAG, "screen reader " + (on ? "on" : "off"));
    }

    // On the worker: the bound service, binding it first if need be.
    private static ScreenReader bound(Context context) {
        sContext = context.getApplicationContext();
        sMain.removeCallbacks(sDisable);
        sMain.postDelayed(sDisable, IDLE_MS);
        ScreenReader s = sInstance;
        if (s != null)
            return s;
        try {
            setEnabled(sContext, true);
        } catch (SecurityException e) {
            Log.w(Link.TAG, "cannot turn on the screen reader: " + e.getMessage());
            return null;
        }
        long deadline = System.currentTimeMillis() + BIND_WAIT_MS;
        while ((s = sInstance) == null && System.currentTimeMillis() < deadline) {
            try {
                Thread.sleep(50);
            } catch (InterruptedException e) {
                return null;
            }
        }
        return s;
    }

    private static JSONObject reply(String type, JSONObject m) throws JSONException {
        return new JSONObject().put("t", type).put("req", m.optLong("req"));
    }

    private static JSONObject failed(String type, JSONObject m, String error) {
        try {
            return reply(type, m).put("ok", false).put("error", error);
        } catch (JSONException e) {
            return new JSONObject();
        }
    }

    private static String cut(CharSequence s, int max) {
        if (s == null)
            return "";
        String t = s.toString();
        return t.length() > max ? t.substring(0, max) : t;
    }

    private static JSONObject readNow(Context context, JSONObject m) {
        String pkg = m.optString("pkg");
        ScreenReader s = bound(context);
        if (s == null)
            return failed("screen", m, "screen access is off in Android");
        AccessibilityNodeInfo root = null;
        CharSequence title = null;
        for (AccessibilityWindowInfo w : s.getWindows()) {
            if (w.getType() != AccessibilityWindowInfo.TYPE_APPLICATION)
                continue;
            AccessibilityNodeInfo r = w.getRoot();
            if (r == null || !pkg.contentEquals(r.getPackageName() == null ? "" : r.getPackageName()))
                continue;
            if (root == null || w.isFocused() || w.isActive()) {
                root = r;
                title = w.getTitle();
            }
            if (w.isFocused() || w.isActive())
                break;
        }
        if (root == null)
            return failed("screen", m, "the app shows no window Android can read");

        sNodes.clear();
        sPkg = pkg;
        sSnap = sSnap >= Integer.MAX_VALUE ? 1 : sSnap + 1;
        JSONArray nodes = new JSONArray();
        ArrayDeque<AccessibilityNodeInfo> stack = new ArrayDeque<>();
        ArrayDeque<Integer> depths = new ArrayDeque<>();
        stack.push(root);
        depths.push(0);
        int visits = 0;
        try {
            while (!stack.isEmpty() && visits++ < MAX_VISITS && sNodes.size() < MAX_NODES) {
                AccessibilityNodeInfo n = stack.pop();
                int depth = depths.pop();
                if (!n.isVisibleToUser())
                    continue;
                CharSequence text = n.isPassword() ? null : n.getText();
                CharSequence desc = n.getContentDescription();
                CharSequence hint = n.getHintText();
                int f = (n.isClickable() ? 1 : 0) | (n.isLongClickable() ? 2 : 0)
                        | (n.isEditable() ? 4 : 0) | (n.isCheckable() ? 8 : 0)
                        | (n.isChecked() ? 16 : 0) | (n.isSelected() ? 32 : 0)
                        | (n.isEnabled() ? 0 : 64) | (n.isFocused() ? 128 : 0)
                        | (n.isScrollable() ? 256 : 0) | (n.isPassword() ? 512 : 0)
                        | (n.isHeading() ? 1024 : 0);
                boolean shown = (text != null && text.length() > 0) || (desc != null && desc.length() > 0)
                        || (hint != null && hint.length() > 0) || (f & (1 | 2 | 4 | 8 | 256 | 512)) != 0;
                int childDepth = depth;
                if (shown) {
                    JSONObject o = new JSONObject().put("i", sNodes.size()).put("d", depth)
                            .put("c", cut(n.getClassName(), 128)).put("f", f);
                    if (text != null && text.length() > 0)
                        o.put("t", cut(text, 1000));
                    if (desc != null && desc.length() > 0)
                        o.put("cd", cut(desc, 500));
                    if (hint != null && hint.length() > 0)
                        o.put("h", cut(hint, 200));
                    if (n.getViewIdResourceName() != null)
                        o.put("v", cut(n.getViewIdResourceName(), 128));
                    nodes.put(o);
                    sNodes.add(n);
                    childDepth = depth + 1;
                }
                for (int i = n.getChildCount() - 1; i >= 0; i--) {
                    AccessibilityNodeInfo c = n.getChild(i);
                    if (c != null) {
                        stack.push(c);
                        depths.push(childDepth);
                    }
                }
            }
            return reply("screen", m).put("ok", true).put("pkg", pkg).put("snap", sSnap)
                    .put("title", cut(title, 256)).put("nodes", nodes);
        } catch (JSONException e) {
            return failed("screen", m, "the screen could not be described");
        }
    }

    private static JSONObject actNow(Context context, JSONObject m) {
        String pkg = m.optString("pkg");
        int index = m.optInt("node", -1);
        if (bound(context) == null)
            return failed("ui_done", m, "screen access is off in Android");
        if (!pkg.equals(sPkg) || m.optLong("snap") != sSnap || index < 0 || index >= sNodes.size())
            return failed("ui_done", m, "the screen changed: read it again");
        AccessibilityNodeInfo n = sNodes.get(index);
        if (!n.refresh() || !n.isVisibleToUser() || n.getPackageName() == null
                || !pkg.contentEquals(n.getPackageName()))
            return failed("ui_done", m, "that element is gone: read the screen again");
        boolean done;
        if ("click".equals(m.optString("act"))) {
            AccessibilityNodeInfo target = n;
            for (int up = 0; target != null && !target.isClickable() && up < 8; up++)
                target = target.getParent();
            if (target == null || !target.isClickable() || !target.isEnabled())
                return failed("ui_done", m, "that element cannot be tapped");
            done = target.performAction(AccessibilityNodeInfo.ACTION_CLICK);
        } else if ("set_text".equals(m.optString("act"))) {
            if (!n.isEditable() || n.isPassword())
                return failed("ui_done", m, "that is not a text field Pilot may fill");
            Bundle args = new Bundle();
            args.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                    m.optString("text"));
            done = n.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args);
        } else {
            return failed("ui_done", m, "unknown act");
        }
        if (!done)
            return failed("ui_done", m, "the app did not take it");
        try {
            return reply("ui_done", m).put("ok", true);
        } catch (JSONException e) {
            return new JSONObject();
        }
    }
}
