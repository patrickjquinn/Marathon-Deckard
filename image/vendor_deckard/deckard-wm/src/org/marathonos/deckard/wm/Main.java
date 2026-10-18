package org.marathonos.deckard.wm;

import android.app.TaskStackListener;
import android.content.ComponentName;
import android.graphics.Rect;
import android.hardware.display.DisplayManager;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.util.Log;

import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.List;

/**
 * Keeps every app task on Android's display as a freeform task that fills the
 * display. Waydroid gives each task its own Wayland window the size of the
 * display, so a task that fills the display fills its Marathon window.
 */
public final class Main {
    private static final String TAG = "DeckardWm";
    private static final int DISPLAY = 0;
    private static final int WINDOWING_MODE_FULLSCREEN = 1;
    private static final int WINDOWING_MODE_FREEFORM = 5;
    private static final int ACTIVITY_TYPE_UNDEFINED = 0;
    private static final int ACTIVITY_TYPE_STANDARD = 1;
    private static final long SETTLE_MS = 30;

    private static Object atm;
    private static Method getAllRootTaskInfos;
    private static Object windowOrganizer;
    private static Method applyTransaction;
    private static Class<?> wctClass;
    private static Method wctSetWindowingMode;
    private static Method wctSetBounds;
    private static Object displayManager;
    private static Method getDisplayInfo;
    private static Handler handler;

    private static final Runnable FIT = Main::fitAll;

    public static void main(String[] args) throws Exception {
        Looper.prepareMainLooper();
        handler = new Handler(Looper.myLooper());

        Method getService = Class.forName("android.os.ServiceManager")
                .getMethod("getService", String.class);
        IBinder binder;
        while ((binder = (IBinder) getService.invoke(null, "activity_task")) == null) {
            Thread.sleep(500);
        }
        // init restarts us against the new system_server.
        binder.linkToDeath(() -> System.exit(0), 0);
        atm = Class.forName("android.app.IActivityTaskManager$Stub")
                .getMethod("asInterface", IBinder.class).invoke(null, binder);
        getAllRootTaskInfos = atm.getClass().getMethod("getAllRootTaskInfos");
        windowOrganizer = atm.getClass().getMethod("getWindowOrganizerController").invoke(atm);
        wctClass = Class.forName("android.window.WindowContainerTransaction");
        Class<?> token = Class.forName("android.window.WindowContainerToken");
        wctSetWindowingMode = wctClass.getMethod("setWindowingMode", token, int.class);
        wctSetBounds = wctClass.getMethod("setBounds", token, Rect.class);
        applyTransaction = windowOrganizer.getClass().getMethod("applyTransaction", wctClass);

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
        };
        Class<?> listenerType = Class.forName("android.app.ITaskStackListener");
        atm.getClass().getMethod("registerTaskStackListener", listenerType).invoke(atm, tasks);

        fitAll();
        Looper.loop();
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
        try {
            Rect full = displayBounds();
            if (full == null) return;
            List<?> roots = (List<?>) getAllRootTaskInfos.invoke(atm);
            for (Object root : roots) fit(root, full);
        } catch (Throwable t) {
            Log.w(TAG, "fit failed", t);
        }
    }

    private static void fit(Object root, Rect full) throws Exception {
        if (intField(root, "displayId") != DISPLAY) return;
        Object windowConfig = windowConfiguration(root);
        int type = (int) windowConfig.getClass().getMethod("getActivityType").invoke(windowConfig);
        if (type != ACTIVITY_TYPE_STANDARD && type != ACTIVITY_TYPE_UNDEFINED) return;
        int mode = (int) windowConfig.getClass().getMethod("getWindowingMode").invoke(windowConfig);

        Rect bounds = (Rect) root.getClass().getField("bounds").get(root);
        if (mode == WINDOWING_MODE_FREEFORM && full.equals(bounds)) return;
        if (mode != WINDOWING_MODE_FREEFORM && mode != WINDOWING_MODE_FULLSCREEN) return;

        // A fullscreen task hides every task below it, and Waydroid then
        // destroys their windows, so fullscreen tasks become freeform too.
        Object token = root.getClass().getField("token").get(root);
        Object wct = wctClass.newInstance();
        wctSetWindowingMode.invoke(wct, token, WINDOWING_MODE_FREEFORM);
        wctSetBounds.invoke(wct, token, full);
        applyTransaction.invoke(windowOrganizer, wct);
    }

    private static Object windowConfiguration(Object taskInfo) throws Exception {
        Field f = taskInfo.getClass().getField("configuration");
        Object config = f.get(taskInfo);
        return config.getClass().getField("windowConfiguration").get(config);
    }

    private static int intField(Object o, String name) throws Exception {
        return o.getClass().getField(name).getInt(o);
    }
}
