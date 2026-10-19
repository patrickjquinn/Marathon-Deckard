package org.marathonos.deckard.permission;

import android.app.Activity;
import android.app.Application;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.graphics.drawable.Icon;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Parcel;
import android.os.Process;
import android.os.ResultReceiver;
import android.text.Spanned;
import android.text.style.ClickableSpan;
import android.util.Log;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;
import android.widget.FrameLayout;

import com.android.permissioncontroller.permission.ui.GrantPermissionsViewHandler;
import com.android.permissioncontroller.permission.ui.handheld.GrantPermissionsViewHandlerImpl;
import com.android.permissioncontroller.permission.utils.Utils;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * GrantPermissionsActivity's view, put in place of the handheld one by
 * Deckard's PermissionController patch (docs/permissions.md). Android still
 * decides what to ask and applies the answer; this hands each question to
 * Marathon's prompt through DeckardSystem and answers with the button the
 * person's choice stands for. When Marathon can't ask, Android's own dialog
 * is shown instead.
 */
public final class MarathonGrantView implements GrantPermissionsViewHandler {
    private static final String TAG = "DeckardPermissions";
    private static final Uri PROMPTS = Uri.parse("content://org.marathonos.deckard.system.permissions");

    // GrantPermissionsViewHandler's results and GrantPermissionsActivity's button and location indices.
    private static final int CANCELED = -1;
    private static final int GRANTED_ALWAYS = 0;
    private static final int GRANTED_FOREGROUND_ONLY = 1;
    private static final int DENIED = 2;
    private static final int DENIED_DO_NOT_ASK_AGAIN = 3;
    private static final int GRANTED_ONE_TIME = 4;
    private static final int ALLOW = 0;
    private static final int ALLOW_FOREGROUND = 2;
    private static final int DENY = 3;
    private static final int DENY_AND_DONT_ASK_AGAIN = 4;
    private static final int ALLOW_ONE_TIME = 5;
    private static final int NO_UPGRADE_AND_DONT_ASK_AGAIN = 7;
    private static final int NO_UPGRADE_OT_AND_DONT_ASK_AGAIN = 9;
    private static final int LINK_TO_SETTINGS = 10;
    private static final int LOCATION_ACCURACY = 0;
    private static final int FINE_RADIO = 1;
    private static final int RADIO_GROUP = 3;
    private static final int FINE_ONLY = 4;
    private static final int COARSE_ONLY = 5;
    private static final String FINE = "android.permission.ACCESS_FINE_LOCATION";
    private static final String COARSE = "android.permission.ACCESS_COARSE_LOCATION";

    private final Activity mActivity;
    private final String mPackage;
    private final ResultListener mListener;
    private final GrantPermissionsViewHandlerImpl mStock;
    private final Handler mMain = new Handler(Looper.getMainLooper());
    private final Set<String> mLinked = new HashSet<>();
    private View mStockView;
    private boolean mFallback;

    private String mGroup;
    private int mCount;
    private int mIndex;
    private Icon mIcon;
    private CharSequence mMessage;
    private CharSequence mDetail;
    private boolean[] mButtons = new boolean[11];
    private boolean[] mLocation = new boolean[6];
    private String mPendingId;
    private String mPendingSignature;

    public MarathonGrantView(Activity activity, String pkg, ResultListener listener) {
        mActivity = activity;
        mPackage = pkg;
        mListener = listener;
        mStock = new GrantPermissionsViewHandlerImpl(activity, pkg, Process.myUserHandle()).setResultListener(listener);
        activity.registerActivityLifecycleCallbacks(new Application.ActivityLifecycleCallbacks() {
            @Override public void onActivityCreated(Activity a, Bundle b) {}
            @Override public void onActivityStarted(Activity a) {}
            @Override public void onActivityResumed(Activity a) {}
            @Override public void onActivityPaused(Activity a) {}
            @Override public void onActivityStopped(Activity a) {}
            @Override public void onActivitySaveInstanceState(Activity a, Bundle b) {}
            @Override public void onActivityDestroyed(Activity a) {
                cancelPending();
            }
        });
    }

    @Override
    public View createView() {
        FrameLayout root = new FrameLayout(mActivity);
        mStockView = mStock.createView();
        mStockView.setVisibility(View.INVISIBLE);
        root.addView(mStockView);
        return root;
    }

    @Override
    public void updateWindowAttributes(WindowManager.LayoutParams params) {
        mStock.updateWindowAttributes(params);
        if (!mFallback)
            params.flags &= ~WindowManager.LayoutParams.FLAG_DIM_BEHIND;
    }

    @Override
    public void onBlurEnabledChanged(Window window, boolean enabled) {
        if (mFallback)
            mStock.onBlurEnabledChanged(window, enabled);
    }

    @Override
    public void saveInstanceState(Bundle state) {
        mStock.saveInstanceState(state);
        state.putBoolean("deckard_fallback", mFallback);
    }

    @Override
    public void loadInstanceState(Bundle state) {
        if (state.getBoolean("deckard_fallback"))
            fallBack();
        mStock.loadInstanceState(state);
    }

    @Override
    public void onBackPressed() {
        if (mFallback || mGroup == null) {
            mStock.onBackPressed();
            return;
        }
        cancelPending();
        mListener.onPermissionGrantResult(mGroup, CANCELED);
    }

    @Override
    public void updateUi(String group, int count, int index, Icon icon, CharSequence message,
            CharSequence detail, boolean[] buttons, boolean[] location) {
        mGroup = group;
        mCount = count;
        mIndex = index;
        mIcon = icon;
        mMessage = message;
        mDetail = detail;
        mButtons = Arrays.copyOf(buttons, 11);
        mLocation = Arrays.copyOf(location, 6);
        if (mFallback) {
            mStock.updateUi(group, count, index, icon, message, detail, buttons, location);
            return;
        }
        mActivity.getWindow().clearFlags(WindowManager.LayoutParams.FLAG_DIM_BEHIND);

        boolean canAllow = mButtons[ALLOW] || mButtons[ALLOW_FOREGROUND] || mButtons[ALLOW_ONE_TIME];
        if (!canAllow && mButtons[LINK_TO_SETTINGS]) {
            // Android asks for "all the time" only through its settings page,
            // which DeckardSystem answers with Marathon's prompt. Asked again
            // after that, it is a no.
            if (mLinked.add(group) && openLink())
                return;
            boolean dontAsk = mButtons[NO_UPGRADE_AND_DONT_ASK_AGAIN] || mButtons[NO_UPGRADE_OT_AND_DONT_ASK_AGAIN];
            answerLater(dontAsk ? DENIED_DO_NOT_ASK_AGAIN : DENIED, null);
            return;
        }

        String signature = group + Arrays.toString(mButtons) + Arrays.toString(mLocation);
        if (mPendingId != null && signature.equals(mPendingSignature))
            return;
        cancelPending();
        String id = ask(group);
        if (id == null) {
            fallBack();
            return;
        }
        mPendingId = id;
        mPendingSignature = signature;
    }

    private String ask(String group) {
        List<String> perms = groupPermissions(group);
        if (perms.isEmpty())
            return null;
        Bundle extras = new Bundle();
        extras.putString("pkg", mPackage);
        extras.putStringArrayList("perms", new ArrayList<>(perms));
        extras.putString("kind", mLocation[FINE_ONLY] ? "precise" : "foreground");
        extras.putBoolean("once", mButtons[ALLOW_ONE_TIME]);
        extras.putString("accuracy", mLocation[RADIO_GROUP] ? "choice" : mLocation[COARSE_ONLY] ? "approximate" : "");
        extras.putBoolean("preciseDefault", mLocation[RADIO_GROUP] ? mLocation[FINE_RADIO] : mLocation[FINE_ONLY]);
        extras.putParcelable("receiver", forIpc(new ResultReceiver(mMain) {
            @Override
            protected void onReceiveResult(int code, Bundle data) {
                answered(data);
            }
        }));
        try {
            Bundle reply = mActivity.getContentResolver().call(PROMPTS, "ask", null, extras);
            return reply != null ? reply.getString("id") : null;
        } catch (RuntimeException e) {
            Log.w(TAG, "Marathon can't ask: " + e);
            return null;
        }
    }

    private void answered(Bundle data) {
        if (data == null || mPendingId == null || !mPendingId.equals(data.getString("id")))
            return;
        mPendingId = null;
        mPendingSignature = null;
        String answer = data.getString("answer", "cancel");
        boolean precise = data.getBoolean("precise");
        switch (answer) {
            case "allow":
                answerLater(allowResult(), affected(precise));
                break;
            case "once":
                answerLater(mButtons[ALLOW_ONE_TIME] ? GRANTED_ONE_TIME : allowResult(), affected(precise));
                break;
            case "deny":
                answerLater(mButtons[DENY_AND_DONT_ASK_AGAIN] ? DENIED_DO_NOT_ASK_AGAIN : DENIED, affected(precise));
                break;
            case "unsupported":
                fallBack();
                break;
            default:
                answerLater(CANCELED, null);
                break;
        }
    }

    private int allowResult() {
        if (mButtons[ALLOW])
            return GRANTED_ALWAYS;
        if (mButtons[ALLOW_FOREGROUND])
            return GRANTED_FOREGROUND_ONLY;
        return mButtons[ALLOW_ONE_TIME] ? GRANTED_ONE_TIME : CANCELED;
    }

    // As the handheld dialog passes them: the location permissions chosen.
    private List<String> affected(boolean precise) {
        if (!mLocation[LOCATION_ACCURACY])
            return null;
        if (mLocation[RADIO_GROUP])
            return precise ? Arrays.asList(FINE, COARSE) : Arrays.asList(COARSE);
        if (mLocation[FINE_ONLY])
            return Arrays.asList(FINE);
        if (mLocation[COARSE_ONLY])
            return Arrays.asList(COARSE);
        return null;
    }

    private void answerLater(int result, List<String> affected) {
        String group = mGroup;
        mMain.post(() -> {
            if (result == CANCELED)
                mListener.onPermissionGrantResult(group, CANCELED);
            else
                mListener.onPermissionGrantResult(group, affected, result);
        });
    }

    private boolean openLink() {
        if (!(mDetail instanceof Spanned))
            return false;
        Spanned detail = (Spanned) mDetail;
        ClickableSpan[] links = detail.getSpans(0, detail.length(), ClickableSpan.class);
        if (links.length == 0)
            return false;
        ClickableSpan link = links[0];
        mMain.post(() -> link.onClick(mStockView));
        return true;
    }

    private void fallBack() {
        if (mFallback)
            return;
        Log.i(TAG, "Android's dialog for " + mPackage + " " + mGroup);
        mFallback = true;
        cancelPending();
        mActivity.getWindow().addFlags(WindowManager.LayoutParams.FLAG_DIM_BEHIND);
        if (mStockView != null)
            mStockView.setVisibility(View.VISIBLE);
        if (mGroup != null)
            mStock.updateUi(mGroup, mCount, mIndex, mIcon, mMessage, mDetail, mButtons, mLocation);
    }

    private void cancelPending() {
        if (mPendingId == null)
            return;
        Bundle extras = new Bundle();
        extras.putString("id", mPendingId);
        mPendingId = null;
        mPendingSignature = null;
        try {
            mActivity.getContentResolver().call(PROMPTS, "cancel", null, extras);
        } catch (RuntimeException e) {
            Log.w(TAG, "cancel: " + e);
        }
    }

    // The group's platform permissions this app asks for.
    private List<String> groupPermissions(String group) {
        List<String> perms = new ArrayList<>();
        List<String> platform = Utils.getPlatformPermissionNamesOfGroup(group);
        try {
            PackageInfo info = mActivity.getPackageManager().getPackageInfo(mPackage, PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS));
            if (info.requestedPermissions != null)
                for (String p : info.requestedPermissions)
                    if (platform.contains(p))
                        perms.add(p);
        } catch (PackageManager.NameNotFoundException e) {
            return perms;
        }
        return perms;
    }

    // A ResultReceiver of the base class: a subclass can't be unparcelled
    // in DeckardSystem's process.
    private static ResultReceiver forIpc(ResultReceiver receiver) {
        Parcel parcel = Parcel.obtain();
        try {
            receiver.writeToParcel(parcel, 0);
            parcel.setDataPosition(0);
            return ResultReceiver.CREATOR.createFromParcel(parcel);
        } finally {
            parcel.recycle();
        }
    }
}
