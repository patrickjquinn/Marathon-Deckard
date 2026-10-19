package org.marathonos.deckard.system;

import android.app.Activity;
import android.content.ComponentName;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.pm.PermissionInfo;
import android.os.Bundle;
import android.os.Process;
import android.os.UserHandle;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * Android's per-app permission page (MANAGE_APP_PERMISSION). When
 * GrantPermissionsActivity sends an app there to allow a permission "all
 * the time" (background location, Android 11 and later), this asks with
 * Marathon's prompt and applies the answer as that page would; it answers
 * GrantPermissionsActivity as the page does. Opened from anywhere else, it
 * shows the app's page in Marathon Settings.
 */
public final class BackgroundAccess extends Activity {
    private static final String CONTROLLER = "com.android.permissioncontroller";
    private static final String GRANT_ACTIVITY = CONTROLLER + ".permission.ui.GrantPermissionsActivity";
    private static final String EXTRA_GROUP = "android.intent.extra.PERMISSION_GROUP_NAME";
    private static final String EXTRA_CALLER = CONTROLLER + ".extra.CALLER_NAME";
    private static final String RESULT_INTERACTED = CONTROLLER + ".extra.RESULT_PERMISSION_INTERACTED";
    private static final String RESULT_PERMISSION = CONTROLLER + ".extra.PERMISSION_RESULT";
    private static final int FLAG_USER_SET = 1;
    private static final int FLAG_USER_FIXED = 2;
    private static final int FLAG_POLICY_FIXED = 4;
    private static final int FLAG_SYSTEM_FIXED = 16;
    private static final int STOCK_PAGE = 1;

    private String mPkg;
    private String mGroup;
    private String mId;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Intent intent = getIntent();
        mPkg = intent.getStringExtra(Intent.EXTRA_PACKAGE_NAME);
        mGroup = intent.getStringExtra(EXTRA_GROUP);
        if (mPkg == null || mGroup == null || !GRANT_ACTIVITY.equals(intent.getStringExtra(EXTRA_CALLER))
                || !CONTROLLER.equals(getCallingPackage())) {
            openSettings();
            finish();
            return;
        }
        if (state != null) {
            finish();
            return;
        }
        getPackageManager().getPlatformPermissionsForGroup(mGroup, getMainExecutor(), this::ask);
    }

    private void ask(List<String> groupPermissions) {
        List<String> background = background(groupPermissions);
        if (background.isEmpty()) {
            finishWith(null, 0);
            return;
        }
        mId = PermissionPrompts.newId();
        if (!PermissionPrompts.ask(mId, mPkg, background, "background", false, "", false,
                (answer, precise) -> runOnUiThread(() -> answered(background, answer)))) {
            mId = null;
            stockPage();
        }
    }

    private void answered(List<String> background, String answer) {
        mId = null;
        if ("unsupported".equals(answer)) {
            stockPage();
            return;
        }
        if (!"allow".equals(answer) && !"deny".equals(answer)) {
            finishWith(null, 0);
            return;
        }
        boolean allow = "allow".equals(answer);
        try {
            for (String p : background)
                apply(p, allow);
        } catch (ReflectiveOperationException | RuntimeException e) {
            Log.w(HostLink.TAG, "background " + mGroup + " for " + mPkg + ": " + e);
        }
        finishWith(mGroup, allow ? 0 : 2);
    }

    // The background permissions of the group's granted foreground ones that the app asks for and lacks.
    private List<String> background(List<String> groupPermissions) {
        List<String> out = new ArrayList<>();
        PackageManager pm = getPackageManager();
        try {
            PackageInfo pi = pm.getPackageInfo(mPkg, PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS));
            List<String> requested = pi.requestedPermissions == null ? new ArrayList<>() : Arrays.asList(pi.requestedPermissions);
            for (String p : groupPermissions) {
                if (!requested.contains(p) || pm.checkPermission(p, mPkg) != PackageManager.PERMISSION_GRANTED)
                    continue;
                PermissionInfo info = pm.getPermissionInfo(p, 0);
                String bg = (String) PermissionInfo.class.getField("backgroundPermission").get(info);
                if (bg != null && requested.contains(bg) && !out.contains(bg)
                        && pm.checkPermission(bg, mPkg) != PackageManager.PERMISSION_GRANTED)
                    out.add(bg);
            }
        } catch (PackageManager.NameNotFoundException | ReflectiveOperationException e) {
            out.clear();
        }
        return out;
    }

    // As Android's page: a grant or a refusal the person set; a second refusal is for good.
    private void apply(String perm, boolean allow) throws ReflectiveOperationException {
        PackageManager pm = getPackageManager();
        UserHandle user = Process.myUserHandle();
        int flags = (Integer) PackageManager.class.getMethod("getPermissionFlags", String.class, String.class, UserHandle.class)
                .invoke(pm, perm, mPkg, user);
        if ((flags & (FLAG_POLICY_FIXED | FLAG_SYSTEM_FIXED)) != 0)
            return;
        if (allow)
            PackageManager.class.getMethod("grantRuntimePermission", String.class, String.class, UserHandle.class)
                    .invoke(pm, mPkg, perm, user);
        int set = allow || (flags & FLAG_USER_SET) == 0 ? FLAG_USER_SET : FLAG_USER_SET | FLAG_USER_FIXED;
        PackageManager.class.getMethod("updatePermissionFlags", String.class, String.class, int.class, int.class,
                UserHandle.class).invoke(pm, perm, mPkg, FLAG_USER_SET | FLAG_USER_FIXED, set, user);
    }

    private void finishWith(String interacted, int result) {
        Intent data = new Intent();
        if (interacted != null)
            data.putExtra(RESULT_INTERACTED, interacted).putExtra(RESULT_PERMISSION, result);
        setResult(RESULT_OK, data);
        finish();
    }

    // Without Marathon's prompt: Android's own page, its answer passed on.
    private void stockPage() {
        Intent page = new Intent(getIntent()).setComponent(
                new ComponentName(CONTROLLER, CONTROLLER + ".permission.ui.ManagePermissionsActivity"));
        page.setFlags(0);
        try {
            startActivityForResult(page, STOCK_PAGE);
        } catch (RuntimeException e) {
            Log.w(HostLink.TAG, "no permission page: " + e);
            finishWith(null, 0);
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        if (request != STOCK_PAGE)
            return;
        setResult(result, data);
        finish();
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        if (mId != null && isFinishing())
            PermissionPrompts.cancel(mId);
    }

    private void openSettings() {
        if (mPkg == null)
            return;
        try {
            HostLink.get().send(new JSONObject().put("t", "open").put("kind", "settings")
                    .put("action", "android.settings.APPLICATION_DETAILS_SETTINGS").put("pkg", mPkg));
        } catch (JSONException ignored) {
            // Never thrown for these values.
        }
    }
}
