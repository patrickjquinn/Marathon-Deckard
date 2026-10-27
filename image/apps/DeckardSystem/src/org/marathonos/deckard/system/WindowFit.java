package org.marathonos.deckard.system;

import android.app.TaskStackListener;
import android.content.ComponentName;
import android.graphics.Rect;
import android.hardware.display.DisplayManager;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Keeps every app task on Android's display as a freeform task that fills the
 * display. Waydroid gives each task its own Wayland window the size of the
 * display, so a task that fills the display fills its Marathon window.
 * deckard-wm's logic, run in this app's process instead of its own runtime.
 * On Android 16 tasks stay as Waydroid launches them, fullscreen: a freeform
 * task outside a desktop desk crashes the window manager shell in SystemUI,
 * which Android 16 needs to show the keyboard.
 */
final class WindowFit {
    private static final String TAG = "DeckardWindowFit";
    private static final int DISPLAY = 0;
    private static final int WINDOWING_MODE_FULLSCREEN = 1;
    private static final int WINDOWING_MODE_FREEFORM = 5;
    private static final int ACTIVITY_TYPE_UNDEFINED = 0;
    private static final int ACTIVITY_TYPE_STANDARD = 1;
    private static final int ACTIVITY_TYPE_HOME = 2;
    private static final long SETTLE_MS = 30;
    private static final long RETRY_MS = 2000;
    private static final long RECLAIM_MS = 1000;
    private static final boolean FIT_TASKS = Build.VERSION.SDK_INT < 36;

    private static Object atm;
    private static Method getAllRootTaskInfos;
    private static Object windowOrganizer;
    private static Method applyTransaction;
    private static Class<?> wctClass;
    private static Method wctSetWindowingMode;
    private static Method wctSetBounds;
    private static Method wctReorder;
    private static Method setFocusedTask;
    private static Object displayManager;
    private static Method getDisplayInfo;
    private static Handler handler;
    private static final Map<Integer, Long> fitted = new HashMap<>();
    private static Rect lastDisplay;

    private static final Runnable FIT = WindowFit::fitAll;

    static void start() {
        handler = new Handler(Looper.getMainLooper());
        try {
            init();
        } catch (Throwable t) {
            Log.w(TAG, "window fit unavailable", t);
        }
    }

    private static void init() throws Exception {
        IBinder binder = (IBinder) Class.forName("android.os.ServiceManager")
                .getMethod("getService", String.class).invoke(null, "activity_task");
        atm = Class.forName("android.app.IActivityTaskManager$Stub")
                .getMethod("asInterface", IBinder.class).invoke(null, binder);
        getAllRootTaskInfos = atm.getClass().getMethod("getAllRootTaskInfos");
        windowOrganizer = atm.getClass().getMethod("getWindowOrganizerController").invoke(atm);
        wctClass = Class.forName("android.window.WindowContainerTransaction");
        Class<?> token = Class.forName("android.window.WindowContainerToken");
        wctSetWindowingMode = wctClass.getMethod("setWindowingMode", token, int.class);
        wctSetBounds = wctClass.getMethod("setBounds", token, Rect.class);
        wctReorder = wctClass.getMethod("reorder", token, boolean.class);
        applyTransaction = windowOrganizer.getClass().getMethod("applyTransaction", wctClass);
        setFocusedTask = atm.getClass().getMethod("setFocusedTask", int.class);

        Class<?> dmg = Class.forName("android.hardware.display.DisplayManagerGlobal");
        displayManager = dmg.getMethod("getInstance").invoke(null);
        getDisplayInfo = dmg.getMethod("getDisplayInfo", int.class);

        DisplayManager.DisplayListener displays = new DisplayManager.DisplayListener() {
            @Override public void onDisplayAdded(int displayId) {}
            @Override public void onDisplayRemoved(int displayId) {}
            @Override public void onDisplayChanged(int displayId) {
                if (displayId == DISPLAY) schedule();
            }
        };
        registerDisplayListener(dmg, displays);

        TaskStackListener tasks = new TaskStackListener() {
            @Override public void onTaskStackChanged() { schedule(); }
            @Override public void onTaskCreated(int taskId, ComponentName name) { schedule(); }
            @Override public void onTaskDisplayChanged(int taskId, int displayId) { schedule(); }
            @Override public void onTaskFocusChanged(int taskId, boolean focused) {
                Log.i(TAG, "task " + taskId + (focused ? " focused" : " unfocused"));
                if (!focused) return;
                handler.post(() -> raise(taskId));
                handler.removeCallbacks(RECLAIM);
                handler.postDelayed(RECLAIM, RECLAIM_MS);
            }
        };
        Class<?> listenerType = Class.forName("android.app.ITaskStackListener");
        atm.getClass().getMethod("registerTaskStackListener", listenerType).invoke(atm, tasks);

        fitAll();
    }

    private static void registerDisplayListener(Class<?> dmg, DisplayManager.DisplayListener l)
            throws Exception {
        for (Method m : dmg.getMethods()) {
            if (!m.getName().equals("registerDisplayListener")) continue;
            Class<?>[] p = m.getParameterTypes();
            if (p.length == 3 && p[2] == long.class) {
                m.invoke(displayManager, l, handler, 1L << 2 /* EVENT_FLAG_DISPLAY_CHANGED */);
                return;
            }
            if (p.length == 2) {
                m.invoke(displayManager, l, handler);
                return;
            }
        }
        Log.w(TAG, "no display listener; size changes are picked up on the next task change");
    }

    private static void schedule() {
        handler.removeCallbacks(FIT);
        handler.postDelayed(FIT, SETTLE_MS);
    }

    private static Rect displayBounds() throws Exception {
        Object info = getDisplayInfo.invoke(displayManager, DISPLAY);
        if (info == null) return null;
        int w = info.getClass().getField("logicalWidth").getInt(info);
        int h = info.getClass().getField("logicalHeight").getInt(info);
        return w > 0 && h > 0 ? new Rect(0, 0, w, h) : null;
    }

    private static void fitAll() {
        if (!FIT_TASKS) return;
        try {
            Rect full = displayBounds();
            if (full == null) return;
            if (!full.equals(lastDisplay)) {
                fitted.clear();
                lastDisplay = full;
            }
            List<?> roots = (List<?>) getAllRootTaskInfos.invoke(atm);
            boolean top = true;
            for (Object root : roots) {
                if (fit(root, full, top)) top = false;
            }
        } catch (Throwable t) {
            Log.w(TAG, "fit failed", t);
        }
    }

    /** Returns whether root is a visible app task (roots come top first). */
    private static boolean fit(Object root, Rect full, boolean top) throws Exception {
        if (intField(root, "displayId") != DISPLAY) return false;
        if (!isVisible(root)) return false;
        // Only tasks that hold activities: not the shell's empty split roots.
        if (intField(root, "numActivities") == 0) return false;
        int[] children = (int[]) root.getClass().getField("childTaskIds").get(root);
        if (children != null && children.length > 1) return false;
        Object windowConfig = windowConfiguration(root);
        int type = (int) windowConfig.getClass().getMethod("getActivityType").invoke(windowConfig);
        if (type != ACTIVITY_TYPE_STANDARD && type != ACTIVITY_TYPE_UNDEFINED) return false;
        int mode = (int) windowConfig.getClass().getMethod("getWindowingMode").invoke(windowConfig);
        if (mode != WINDOWING_MODE_FREEFORM && mode != WINDOWING_MODE_FULLSCREEN) return true;

        Rect bounds = (Rect) root.getClass().getField("bounds").get(root);
        if (mode == WINDOWING_MODE_FREEFORM && full.equals(bounds)) return true;
        // A task that keeps other bounds must not loop us through task-stack
        // callbacks: at most one attempt per task every RETRY_MS.
        int taskId = intField(root, "taskId");
        long now = SystemClock.uptimeMillis();
        Long last = fitted.get(taskId);
        if (last != null && now - last < RETRY_MS) return true;
        fitted.put(taskId, now);

        // A fullscreen task hides every task below it, and Waydroid then
        // destroys their windows, so fullscreen tasks become freeform too.
        // A mode change resumes the focused task's activity, so the task on
        // top is kept on top; hidden tasks are left until they show.
        Object token = root.getClass().getField("token").get(root);
        Object wct = wctClass.newInstance();
        if (mode != WINDOWING_MODE_FREEFORM) {
            wctSetWindowingMode.invoke(wct, token, WINDOWING_MODE_FREEFORM);
            if (top) wctReorder.invoke(wct, token, true);
        }
        wctSetBounds.invoke(wct, token, full);
        applyTransaction.invoke(windowOrganizer, wct);
        Log.i(TAG, "task " + taskId + " " + bounds.toShortString() + " -> " + full.toShortString()
                + (mode != WINDOWING_MODE_FREEFORM ? " (was fullscreen)" : ""));
        return true;
    }

    /**
     * Waydroid sends every touch to one touchscreen in display coordinates,
     * so Android hits whichever task is on top of its own z-order. All app
     * tasks share the display's bounds, so the task Marathon focused (through
     * setFocusedTask) must also be the top one, or a tap lands in a hidden
     * task and focus jumps there.
     */
    private static void raise(int taskId) {
        try {
            List<?> roots = (List<?>) getAllRootTaskInfos.invoke(atm);
            boolean top = true;
            for (Object root : roots) {
                if (intField(root, "displayId") != DISPLAY) continue;
                Object windowConfig = windowConfiguration(root);
                int type = (int) windowConfig.getClass().getMethod("getActivityType").invoke(windowConfig);
                if (type != ACTIVITY_TYPE_STANDARD && type != ACTIVITY_TYPE_UNDEFINED) continue;
                if (holds(root, taskId)) {
                    if (top) return;
                    Object wct = wctClass.newInstance();
                    wctReorder.invoke(wct, root.getClass().getField("token").get(root), true);
                    applyTransaction.invoke(windowOrganizer, wct);
                    Log.i(TAG, "raised focused task " + taskId);
                    return;
                }
                top = false;
            }
        } catch (Throwable t) {
            Log.w(TAG, "raise failed", t);
        }
    }

    private static String pendingFocus;
    private static String marathonFront = "";
    private static final Runnable FOCUS = WindowFit::applyFocus;
    private static final Runnable RECLAIM = WindowFit::reclaim;

    /**
     * Marathon brought deckard.<pkg> to the front, or "" when it shows no
     * Android app. Android normally learns the front app from the
     * hwcomposer's keyboard enter; this covers the times it doesn't, and puts
     * Deckard's home on top when Marathon shows none, so the app last in
     * front pauses as on a phone whose user went home. Debounced, and a
     * no-op when Android already agrees.
     */
    static void focus(String pkg) {
        if (handler == null || pkg == null) return;
        marathonFront = pkg;
        pendingFocus = pkg;
        handler.removeCallbacks(FOCUS);
        handler.postDelayed(FOCUS, 150);
    }

    /**
     * Android moved its focus on its own (an app started an activity) while
     * Marathon shows another Android app, or none: a second later, unless
     * Marathon showed the new one meanwhile, the focus goes back, or every
     * touch lands in the hidden task. With none, Deckard's home goes back on top.
     */
    private static void reclaim() {
        pendingFocus = marathonFront;
        applyFocus();
    }

    /** Deckard's home on top of every app task, focused. */
    private static void showHome() {
        try {
            List<?> roots = (List<?>) getAllRootTaskInfos.invoke(atm);
            for (Object root : roots) {
                if (intField(root, "displayId") != DISPLAY) continue;
                Object windowConfig = windowConfiguration(root);
                int type = (int) windowConfig.getClass().getMethod("getActivityType").invoke(windowConfig);
                if (type != ACTIVITY_TYPE_HOME) continue;
                if (isFocused(root)) return;
                Object wct = wctClass.newInstance();
                wctReorder.invoke(wct, root.getClass().getField("token").get(root), true);
                applyTransaction.invoke(windowOrganizer, wct);
                setFocusedTask.invoke(atm, intField(root, "taskId"));
                Log.i(TAG, "home in front: Marathon shows no Android app");
                return;
            }
            Log.w(TAG, "no home task to put in front");
        } catch (Throwable t) {
            Log.w(TAG, "home failed", t);
        }
    }

    private static void applyFocus() {
        String pkg = pendingFocus;
        if (pkg.isEmpty()) {
            showHome();
            return;
        }
        try {
            List<?> roots = (List<?>) getAllRootTaskInfos.invoke(atm);
            for (Object root : roots) {
                if (intField(root, "displayId") != DISPLAY || !isVisible(root)) continue;
                Object windowConfig = windowConfiguration(root);
                int type = (int) windowConfig.getClass().getMethod("getActivityType").invoke(windowConfig);
                if (type != ACTIVITY_TYPE_STANDARD && type != ACTIVITY_TYPE_UNDEFINED) continue;
                ComponentName base = (ComponentName) root.getClass().getField("baseActivity").get(root);
                if (base == null || !pkg.equals(base.getPackageName())) continue;
                if (isFocused(root)) return;
                int taskId = intField(root, "taskId");
                setFocusedTask.invoke(atm, taskId);
                // Focus alone leaves the window order: touches go by that.
                Object wct = wctClass.newInstance();
                wctReorder.invoke(wct, root.getClass().getField("token").get(root), true);
                applyTransaction.invoke(windowOrganizer, wct);
                Log.i(TAG, "focused task " + taskId + " for " + pkg);
                return;
            }
        } catch (Throwable t) {
            Log.w(TAG, "focus failed", t);
        }
    }

    private static boolean holds(Object root, int taskId) throws Exception {
        if (intField(root, "taskId") == taskId) return true;
        int[] children = (int[]) root.getClass().getField("childTaskIds").get(root);
        if (children != null)
            for (int c : children)
                if (c == taskId) return true;
        return false;
    }

    private static Object windowConfiguration(Object taskInfo) throws Exception {
        Field f = taskInfo.getClass().getField("configuration");
        Object config = f.get(taskInfo);
        return config.getClass().getField("windowConfiguration").get(config);
    }

    private static boolean isFocused(Object taskInfo) {
        try {
            return taskInfo.getClass().getField("isFocused").getBoolean(taskInfo);
        } catch (ReflectiveOperationException e) {
            return false;
        }
    }

    private static boolean isVisible(Object taskInfo) throws Exception {
        try {
            return taskInfo.getClass().getField("visible").getBoolean(taskInfo);
        } catch (NoSuchFieldException e) {
            return taskInfo.getClass().getField("isVisible").getBoolean(taskInfo);
        }
    }

    private static int intField(Object o, String name) throws Exception {
        return o.getClass().getField(name).getInt(o);
    }
}
