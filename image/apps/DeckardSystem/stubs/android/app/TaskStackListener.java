package android.app;

import android.content.ComponentName;

/** Compile-time stub of the hidden framework class; the device's is used at run time. */
public class TaskStackListener extends android.os.Binder {
    public void onTaskStackChanged() {}
    public void onTaskCreated(int taskId, ComponentName componentName) {}
    public void onTaskDisplayChanged(int taskId, int newDisplayId) {}
    public void onTaskFocusChanged(int taskId, boolean focused) {}
}
