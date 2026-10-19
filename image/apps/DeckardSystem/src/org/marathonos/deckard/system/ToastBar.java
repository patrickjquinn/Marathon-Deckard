package org.marathonos.deckard.system;

import android.content.Context;
import android.os.Binder;
import android.os.Bundle;
import android.os.IBinder;
import android.os.Parcel;
import android.text.TextUtils;
import android.util.Log;

import java.lang.reflect.Field;
import java.lang.reflect.Proxy;

/**
 * Text toasts of apps that target Android 11 or later. Android draws them in
 * SystemUI, through IStatusBar.showToast, and the image has no SystemUI;
 * this registers a stand-in IStatusBar with StatusBarManagerService that
 * hands each toast to DeckardBridge, which sends it to deckardd as it does
 * the toasts ToastWatcher sees. Every other IStatusBar call is a oneway
 * notice for a status bar there isn't, and is dropped, as it was with no
 * IStatusBar registered.
 */
final class ToastBar extends Binder {
    private static final String TAG = "DeckardToastBar";
    private static final String DESCRIPTOR = "com.android.internal.statusbar.IStatusBar";

    private final int mShowToast;

    private ToastBar(int showToast) {
        mShowToast = showToast;
        attachInterface(null, DESCRIPTOR);
    }

    static void start() {
        try {
            Class<?> bar = Class.forName(DESCRIPTOR);
            Field code = Class.forName(DESCRIPTOR + "$Stub").getDeclaredField("TRANSACTION_showToast");
            code.setAccessible(true);
            ToastBar binder = new ToastBar(code.getInt(null));
            Object callbacks = Proxy.newProxyInstance(bar.getClassLoader(), new Class<?>[] {bar},
                    (proxy, method, args) -> "asBinder".equals(method.getName()) ? binder : null);
            IBinder service = (IBinder) Class.forName("android.os.ServiceManager")
                    .getMethod("getService", String.class).invoke(null, "statusbar");
            Object statusBar = Class.forName("com.android.internal.statusbar.IStatusBarService$Stub")
                    .getMethod("asInterface", IBinder.class).invoke(null, service);
            statusBar.getClass().getMethod("registerStatusBar", bar).invoke(statusBar, callbacks);
            Log.i(TAG, "registered");
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "no toasts: " + e);
        }
    }

    @Override
    protected boolean onTransact(int code, Parcel data, Parcel reply, int flags) {
        if (code == INTERFACE_TRANSACTION || code > LAST_CALL_TRANSACTION) {
            try {
                return super.onTransact(code, data, reply, flags);
            } catch (android.os.RemoteException e) {
                return false;
            }
        }
        if (code != mShowToast)
            return true;
        // showToast(int uid, String packageName, IBinder token, CharSequence text, ...)
        data.enforceInterface(DESCRIPTOR);
        data.readInt();
        String pkg = data.readString();
        data.readStrongBinder();
        CharSequence text = data.readInt() != 0 ? TextUtils.CHAR_SEQUENCE_CREATOR.createFromParcel(data) : null;
        if (pkg == null || TextUtils.isEmpty(text))
            return true;
        long token = Binder.clearCallingIdentity();
        try {
            Context c = (Context) Class.forName("android.app.ActivityThread").getMethod("currentApplication").invoke(null);
            Bundle extras = new Bundle();
            extras.putString("pkg", pkg);
            extras.putString("text", text.toString());
            c.getContentResolver().call(Files.AUTHORITY, "toast", null, extras);
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(TAG, "toast from " + pkg + ": " + e);
        } finally {
            Binder.restoreCallingIdentity(token);
        }
        return true;
    }
}
