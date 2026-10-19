package org.marathonos.deckard.system;

import android.app.ActivityManager;
import android.app.AppOpsManager;
import android.app.NotificationManager;
import android.app.admin.DevicePolicyManager;
import android.app.usage.StorageStats;
import android.app.usage.StorageStatsManager;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.pm.PermissionInfo;
import android.content.pm.ResolveInfo;
import android.content.pm.verify.domain.DomainVerificationManager;
import android.content.pm.verify.domain.DomainVerificationUserState;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;
import android.os.Process;
import android.os.UserHandle;
import android.os.storage.StorageManager;
import android.provider.Settings;
import android.util.Log;
import android.view.accessibility.AccessibilityManager;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Marathon Settings' requests for Android apps (protocol/bridge.md,
 * management): details, runtime permissions, notifications, links, battery,
 * force stop, clearing and special access. Runs as the system uid, so
 * Android's own permission checks admit every call; only apps the person
 * installed are changed.
 */
final class Manage {
    private static final String TAG = "DeckardManage";
    private static final ExecutorService WORKER = Executors.newSingleThreadExecutor();

    private static final int FLAG_PERMISSION_USER_SET = 1;
    private static final int FLAG_PERMISSION_USER_FIXED = 2;
    private static final int FLAG_PERMISSION_POLICY_FIXED = 4;
    private static final int FLAG_PERMISSION_SYSTEM_FIXED = 16;
    private static final int FLAG_SUPPORTS_PICTURE_IN_PICTURE = 0x400000;

    // Special access granted through an app op: kind → {permission the app requests, op, op mode when refused}.
    private static final Map<String, String[]> APP_OPS = new HashMap<>();
    static {
        APP_OPS.put("all_files", new String[] {"android.permission.MANAGE_EXTERNAL_STORAGE", "android:manage_external_storage", "2"});
        APP_OPS.put("install_apps", new String[] {"android.permission.REQUEST_INSTALL_PACKAGES", "android:request_install_packages", "2"});
        APP_OPS.put("overlay", new String[] {"android.permission.SYSTEM_ALERT_WINDOW", "android:system_alert_window", "2"});
        APP_OPS.put("write_settings", new String[] {"android.permission.WRITE_SETTINGS", "android:write_settings", "2"});
        APP_OPS.put("usage", new String[] {"android.permission.PACKAGE_USAGE_STATS", "android:get_usage_stats", "1"});
        APP_OPS.put("alarms", new String[] {"android.permission.SCHEDULE_EXACT_ALARM", "android:schedule_exact_alarm", "2"});
        APP_OPS.put("pip", new String[] {null, "android:picture_in_picture", "2"});
    }

    private Manage() {}

    static void handle(JSONObject m) {
        WORKER.execute(() -> {
            JSONObject r;
            String id = m.optString("id");
            try {
                r = run(context(), m.optString("op"), m);
                if (r == null)
                    r = new JSONObject();
                r.put("ok", true);
            } catch (Throwable t) {
                Log.w(TAG, m.optString("op") + ": " + t);
                r = new JSONObject();
                try {
                    r.put("ok", false).put("error", message(t));
                } catch (JSONException ignored) {
                    // Only put() on a fresh object.
                }
            }
            try {
                HostLink.get().send(r.put("t", "managed").put("id", id));
            } catch (JSONException ignored) {
                // Only put() on a fresh object.
            }
        });
    }

    private static String message(Throwable t) {
        while (t.getCause() != null)
            t = t.getCause();
        String m = t.getMessage();
        return m == null ? t.getClass().getSimpleName() : m;
    }

    private static Context context() throws Exception {
        return (Context) Class.forName("android.app.ActivityThread").getMethod("currentApplication").invoke(null);
    }

    private static JSONObject run(Context c, String op, JSONObject m) throws Exception {
        String pkg = m.optString("pkg");
        switch (op) {
            case "info":
                return new JSONObject().put("release", Build.VERSION.RELEASE).put("sdk", Build.VERSION.SDK_INT);
            case "app": return app(c, userApp(c, pkg));
            case "perm": permission(c, userApp(c, pkg), names(m.optJSONArray("permissions")), m.optBoolean("granted")); return null;
            case "perm_apps": return new JSONObject().put("apps", permissionApps(c, names(m.optJSONArray("permissions"))));
            case "notif_apps": return new JSONObject().put("apps", notificationApps(c));
            case "notifications": notifications(c, userApp(c, pkg), m.optBoolean("enabled")); return null;
            case "links": links(c, userApp(c, pkg), m.optBoolean("enabled")); return null;
            case "battery": battery(userApp(c, pkg).packageName, "unrestricted".equals(m.optString("mode"))); return null;
            case "stop": forceStop(c, userApp(c, pkg).packageName); return null;
            case "cache": clearCache(c, userApp(c, pkg).packageName); return null;
            case "clear": clearData(c, userApp(c, pkg).packageName); return null;
            case "special": return new JSONObject().put("apps", special(c, m.optString("kind")));
            case "special_set": setSpecial(c, m.optString("kind"), userApp(c, pkg), m.optBoolean("granted")); return null;
            default: throw new IllegalArgumentException("unknown request");
        }
    }

    /** Only apps the person installed are managed, never Android's own. */
    private static ApplicationInfo userApp(Context c, String pkg) throws Exception {
        ApplicationInfo ai;
        try {
            ai = c.getPackageManager().getApplicationInfo(pkg, PackageManager.ApplicationInfoFlags.of(0));
        } catch (PackageManager.NameNotFoundException e) {
            throw new IllegalArgumentException("not installed");
        }
        if ((ai.flags & ApplicationInfo.FLAG_SYSTEM) != 0)
            throw new IllegalArgumentException("part of the system");
        return ai;
    }

    private static List<ApplicationInfo> userApps(Context c) {
        List<ApplicationInfo> out = new ArrayList<>();
        for (ApplicationInfo ai : c.getPackageManager().getInstalledApplications(PackageManager.ApplicationInfoFlags.of(0))) {
            if ((ai.flags & ApplicationInfo.FLAG_SYSTEM) == 0)
                out.add(ai);
        }
        return out;
    }

    private static Object service(String name, String stub) throws Exception {
        IBinder binder = (IBinder) Class.forName("android.os.ServiceManager")
                .getMethod("getService", String.class).invoke(null, name);
        return Class.forName(stub).getMethod("asInterface", IBinder.class).invoke(null, binder);
    }

    private static Object notificationService() throws Exception {
        return service(Context.NOTIFICATION_SERVICE, "android.app.INotificationManager$Stub");
    }

    private static Object deviceIdle() throws Exception {
        return service("deviceidle", "android.os.IDeviceIdleController$Stub");
    }

    // --- details ---

    private static JSONObject app(Context c, ApplicationInfo ai) throws Exception {
        PackageManager pm = c.getPackageManager();
        String pkg = ai.packageName;
        PackageInfo pi = pm.getPackageInfo(pkg, PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS));
        JSONObject r = new JSONObject().put("pkg", pkg)
                .put("label", pm.getApplicationLabel(ai).toString())
                .put("versionName", pi.versionName == null ? "" : pi.versionName)
                .put("versionCode", pi.getLongVersionCode())
                .put("system", false);
        try {
            StorageStats st = c.getSystemService(StorageStatsManager.class)
                    .queryStatsForPackage(StorageManager.UUID_DEFAULT, pkg, Process.myUserHandle());
            r.put("appBytes", st.getAppBytes()).put("dataBytes", st.getDataBytes()).put("cacheBytes", st.getCacheBytes());
        } catch (Exception e) {
            Log.w(TAG, "sizes of " + pkg + ": " + e);
        }
        Object nm = notificationService();
        r.put("notifications", (Boolean) nm.getClass().getMethod("areNotificationsEnabledForPackage", String.class, int.class)
                .invoke(nm, pkg, ai.uid));
        DomainVerificationUserState links = c.getSystemService(DomainVerificationManager.class)
                .getDomainVerificationUserState(pkg);
        JSONArray domains = new JSONArray();
        if (links != null) {
            for (String host : links.getHostToStateMap().keySet())
                domains.put(host);
        }
        r.put("links", links != null && links.isLinkHandlingAllowed()).put("domains", domains);
        r.put("battery", c.getSystemService(PowerManager.class).isIgnoringBatteryOptimizations(pkg)
                ? "unrestricted" : "optimized");

        // Every runtime permission the app asks for; deckardd names them as Marathon does.
        JSONArray perms = new JSONArray();
        for (String p : requested(c, pi, pi.requestedPermissions == null ? new String[0] : pi.requestedPermissions)) {
            perms.put(new JSONObject().put("name", p)
                    .put("granted", pm.checkPermission(p, pkg) == PackageManager.PERMISSION_GRANTED)
                    .put("fixed", fixed(pm, ai, p)));
        }
        return r.put("perms", perms);
    }

    private static boolean fixed(PackageManager pm, ApplicationInfo ai, String perm) throws Exception {
        return ai.targetSdkVersion < Build.VERSION_CODES.M
                || (flags(pm, perm, ai.packageName) & (FLAG_PERMISSION_SYSTEM_FIXED | FLAG_PERMISSION_POLICY_FIXED)) != 0;
    }

    private static String[] names(JSONArray list) {
        if (list == null)
            return new String[0];
        String[] out = new String[Math.min(list.length(), 64)];
        for (int i = 0; i < out.length; i++)
            out[i] = list.optString(i);
        return out;
    }

    /** The person's apps asking for any of these runtime permissions, and whether they have one. */
    private static JSONArray permissionApps(Context c, String[] perms) throws Exception {
        PackageManager pm = c.getPackageManager();
        JSONArray out = new JSONArray();
        for (ApplicationInfo ai : userApps(c)) {
            PackageInfo pi = pm.getPackageInfo(ai.packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS));
            List<String> asked = requested(c, pi, perms);
            if (asked.isEmpty())
                continue;
            boolean granted = false, fixed = true;
            for (String p : asked) {
                granted |= pm.checkPermission(p, ai.packageName) == PackageManager.PERMISSION_GRANTED;
                fixed &= fixed(pm, ai, p);
            }
            out.put(new JSONObject().put("pkg", ai.packageName).put("label", pm.getApplicationLabel(ai).toString())
                    .put("granted", granted).put("fixed", fixed));
        }
        return out;
    }

    private static JSONArray notificationApps(Context c) throws Exception {
        PackageManager pm = c.getPackageManager();
        Object nm = notificationService();
        JSONArray out = new JSONArray();
        for (ApplicationInfo ai : userApps(c)) {
            boolean on = (Boolean) nm.getClass().getMethod("areNotificationsEnabledForPackage", String.class, int.class)
                    .invoke(nm, ai.packageName, ai.uid);
            out.put(new JSONObject().put("pkg", ai.packageName).put("label", pm.getApplicationLabel(ai).toString())
                    .put("granted", on).put("fixed", false));
        }
        return out;
    }

    /** Those of perms the app asks for that are runtime permissions. */
    private static List<String> requested(Context c, PackageInfo pi, String[] group) {
        List<String> out = new ArrayList<>();
        if (pi.requestedPermissions == null)
            return out;
        Set<String> wanted = new HashSet<>(Arrays.asList(pi.requestedPermissions));
        for (String p : group) {
            if (!wanted.contains(p))
                continue;
            try {
                PermissionInfo info = c.getPackageManager().getPermissionInfo(p, 0);
                if (info.getProtection() == PermissionInfo.PROTECTION_DANGEROUS)
                    out.add(p);
            } catch (PackageManager.NameNotFoundException ignored) {
                // Not defined on this Android.
            }
        }
        return out;
    }

    private static int flags(PackageManager pm, String perm, String pkg) throws Exception {
        return (Integer) PackageManager.class.getMethod("getPermissionFlags", String.class, String.class, UserHandle.class)
                .invoke(pm, perm, pkg, Process.myUserHandle());
    }

    // --- changes ---

    private static void permission(Context c, ApplicationInfo ai, String[] perms, boolean grant) throws Exception {
        if (perms.length == 0)
            throw new IllegalArgumentException("unknown permission");
        PackageManager pm = c.getPackageManager();
        PackageInfo pi = pm.getPackageInfo(ai.packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS));
        UserHandle user = Process.myUserHandle();
        for (String p : requested(c, pi, perms)) {
            if ((flags(pm, p, ai.packageName) & (FLAG_PERMISSION_SYSTEM_FIXED | FLAG_PERMISSION_POLICY_FIXED)) != 0)
                continue;
            PackageManager.class.getMethod(grant ? "grantRuntimePermission" : "revokeRuntimePermission",
                    String.class, String.class, UserHandle.class).invoke(pm, ai.packageName, p, user);
            PackageManager.class.getMethod("updatePermissionFlags", String.class, String.class, int.class, int.class,
                    UserHandle.class).invoke(pm, p, ai.packageName,
                    FLAG_PERMISSION_USER_SET | FLAG_PERMISSION_USER_FIXED, FLAG_PERMISSION_USER_SET, user);
        }
    }

    private static void notifications(Context c, ApplicationInfo ai, boolean enabled) throws Exception {
        Object nm = notificationService();
        nm.getClass().getMethod("setNotificationsEnabledForPackage", String.class, int.class, boolean.class)
                .invoke(nm, ai.packageName, ai.uid, enabled);
    }

    private static void links(Context c, ApplicationInfo ai, boolean enabled) throws Exception {
        DomainVerificationManager dvm = c.getSystemService(DomainVerificationManager.class);
        DomainVerificationManager.class.getMethod("setDomainVerificationLinkHandlingAllowed", String.class, boolean.class)
                .invoke(dvm, ai.packageName, enabled);
    }

    private static void battery(String pkg, boolean unrestricted) throws Exception {
        Object idle = deviceIdle();
        idle.getClass().getMethod(unrestricted ? "addPowerSaveWhitelistApp" : "removePowerSaveWhitelistApp", String.class)
                .invoke(idle, pkg);
    }

    private static void forceStop(Context c, String pkg) throws Exception {
        ActivityManager am = c.getSystemService(ActivityManager.class);
        ActivityManager.class.getMethod("forceStopPackage", String.class).invoke(am, pkg);
    }

    private static void clearCache(Context c, String pkg) throws Exception {
        Class<?> observer = Class.forName("android.content.pm.IPackageDataObserver");
        PackageManager.class.getMethod("deleteApplicationCacheFiles", String.class, observer)
                .invoke(c.getPackageManager(), pkg, null);
    }

    private static void clearData(Context c, String pkg) throws Exception {
        Class<?> observer = Class.forName("android.content.pm.IPackageDataObserver");
        ActivityManager am = c.getSystemService(ActivityManager.class);
        Boolean ok = (Boolean) ActivityManager.class.getMethod("clearApplicationUserData", String.class, observer)
                .invoke(am, pkg, null);
        if (ok == null || !ok)
            throw new IllegalStateException("Android refused");
    }

    // --- special access ---

    private static JSONArray special(Context c, String kind) throws Exception {
        PackageManager pm = c.getPackageManager();
        JSONArray out = new JSONArray();
        for (Map.Entry<ApplicationInfo, Boolean> e : specialApps(c, kind).entrySet()) {
            out.put(new JSONObject().put("pkg", e.getKey().packageName)
                    .put("label", pm.getApplicationLabel(e.getKey()).toString()).put("granted", e.getValue()));
        }
        return out;
    }

    /** The apps that can have this access, and whether they do. */
    private static Map<ApplicationInfo, Boolean> specialApps(Context c, String kind) throws Exception {
        PackageManager pm = c.getPackageManager();
        Map<ApplicationInfo, Boolean> out = new java.util.LinkedHashMap<>();
        Map<String, ApplicationInfo> apps = new HashMap<>();
        for (ApplicationInfo ai : userApps(c))
            apps.put(ai.packageName, ai);
        String[] op = APP_OPS.get(kind);
        if (op != null) {
            AppOpsManager ops = c.getSystemService(AppOpsManager.class);
            for (ApplicationInfo ai : userApps(c)) {
                if (!canHave(pm, ai, op[0]))
                    continue;
                int mode = ops.unsafeCheckOpNoThrow(op[1], ai.uid, ai.packageName);
                boolean granted = mode == AppOpsManager.MODE_ALLOWED || (mode == AppOpsManager.MODE_DEFAULT
                        && op[0] != null && pm.checkPermission(op[0], ai.packageName) == PackageManager.PERMISSION_GRANTED);
                out.put(ai, granted);
            }
            return out;
        }
        switch (kind) {
            case "battery": {
                PowerManager power = c.getSystemService(PowerManager.class);
                for (ApplicationInfo ai : userApps(c))
                    out.put(ai, power.isIgnoringBatteryOptimizations(ai.packageName));
                return out;
            }
            case "dnd": {
                Object nm = notificationService();
                for (ApplicationInfo ai : userApps(c)) {
                    if (canHave(pm, ai, "android.permission.ACCESS_NOTIFICATION_POLICY"))
                        out.put(ai, (Boolean) nm.getClass().getMethod("isNotificationPolicyAccessGrantedForPackage", String.class)
                                .invoke(nm, ai.packageName));
                }
                return out;
            }
            case "notification_listener": {
                NotificationManager nm = c.getSystemService(NotificationManager.class);
                for (Map.Entry<String, List<ComponentName>> e : components(c, kind).entrySet()) {
                    boolean granted = false;
                    for (ComponentName cn : e.getValue())
                        granted |= nm.isNotificationListenerAccessGranted(cn);
                    out.put(apps.get(e.getKey()), granted);
                }
                return out;
            }
            case "accessibility": {
                Set<String> enabled = enabledAccessibility(c);
                for (Map.Entry<String, List<ComponentName>> e : components(c, kind).entrySet()) {
                    boolean granted = false;
                    for (ComponentName cn : e.getValue())
                        granted |= enabled.contains(cn.flattenToString());
                    out.put(apps.get(e.getKey()), granted);
                }
                return out;
            }
            case "device_admin": {
                DevicePolicyManager dpm = c.getSystemService(DevicePolicyManager.class);
                for (Map.Entry<String, List<ComponentName>> e : components(c, kind).entrySet()) {
                    boolean granted = false;
                    for (ComponentName cn : e.getValue())
                        granted |= dpm.isAdminActive(cn);
                    out.put(apps.get(e.getKey()), granted);
                }
                return out;
            }
            default:
                throw new IllegalArgumentException("unknown access");
        }
    }

    private static boolean canHave(PackageManager pm, ApplicationInfo ai, String permission) throws Exception {
        if (permission == null) {
            PackageInfo pi = pm.getPackageInfo(ai.packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_ACTIVITIES));
            if (pi.activities != null) {
                for (ActivityInfo a : pi.activities) {
                    if ((a.flags & FLAG_SUPPORTS_PICTURE_IN_PICTURE) != 0)
                        return true;
                }
            }
            return false;
        }
        PackageInfo pi = pm.getPackageInfo(ai.packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS));
        return pi.requestedPermissions != null && Arrays.asList(pi.requestedPermissions).contains(permission);
    }

    /** Listener, accessibility or admin components of the person's apps, by package. */
    private static Map<String, List<ComponentName>> components(Context c, String kind) {
        PackageManager pm = c.getPackageManager();
        Map<String, ApplicationInfo> apps = new HashMap<>();
        for (ApplicationInfo ai : userApps(c))
            apps.put(ai.packageName, ai);
        List<ResolveInfo> found = new ArrayList<>();
        String bind;
        if ("accessibility".equals(kind)) {
            bind = "android.permission.BIND_ACCESSIBILITY_SERVICE";
            for (android.accessibilityservice.AccessibilityServiceInfo s
                    : c.getSystemService(AccessibilityManager.class).getInstalledAccessibilityServiceList())
                found.add(s.getResolveInfo());
        } else if ("device_admin".equals(kind)) {
            bind = "android.permission.BIND_DEVICE_ADMIN";
            found = pm.queryBroadcastReceivers(new Intent("android.app.action.DEVICE_ADMIN_ENABLED"),
                    PackageManager.ResolveInfoFlags.of(PackageManager.GET_META_DATA));
        } else {
            bind = "android.permission.BIND_NOTIFICATION_LISTENER_SERVICE";
            found = pm.queryIntentServices(new Intent("android.service.notification.NotificationListenerService"),
                    PackageManager.ResolveInfoFlags.of(0));
        }
        Map<String, List<ComponentName>> out = new java.util.LinkedHashMap<>();
        for (ResolveInfo ri : found) {
            String pkg = ri.serviceInfo != null ? ri.serviceInfo.packageName : ri.activityInfo != null ? ri.activityInfo.packageName : null;
            String name = ri.serviceInfo != null ? ri.serviceInfo.name : ri.activityInfo != null ? ri.activityInfo.name : null;
            String perm = ri.serviceInfo != null ? ri.serviceInfo.permission : ri.activityInfo != null ? ri.activityInfo.permission : null;
            ApplicationInfo ai = pkg == null ? null : apps.get(pkg);
            if (ai == null || !bind.equals(perm))
                continue;
            List<ComponentName> list = out.get(pkg);
            if (list == null)
                out.put(pkg, list = new ArrayList<>());
            list.add(new ComponentName(pkg, name));
        }
        return out;
    }

    private static Set<String> enabledAccessibility(Context c) {
        Set<String> out = new LinkedHashSet<>();
        String s = Settings.Secure.getString(c.getContentResolver(), Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES);
        if (s != null) {
            for (String part : s.split(":")) {
                ComponentName cn = ComponentName.unflattenFromString(part);
                if (cn != null)
                    out.add(cn.flattenToString());
            }
        }
        return out;
    }

    private static void setSpecial(Context c, String kind, ApplicationInfo ai, boolean grant) throws Exception {
        boolean eligible = false;
        for (ApplicationInfo a : specialApps(c, kind).keySet())
            eligible |= a.packageName.equals(ai.packageName);
        if (!eligible)
            throw new IllegalArgumentException("this app can't have that access");
        String[] op = APP_OPS.get(kind);
        if (op != null) {
            AppOpsManager ops = c.getSystemService(AppOpsManager.class);
            AppOpsManager.class.getMethod("setMode", String.class, int.class, String.class, int.class)
                    .invoke(ops, op[1], ai.uid, ai.packageName, grant ? AppOpsManager.MODE_ALLOWED : Integer.parseInt(op[2]));
            return;
        }
        switch (kind) {
            case "battery":
                battery(ai.packageName, grant);
                return;
            case "dnd": {
                NotificationManager nm = c.getSystemService(NotificationManager.class);
                NotificationManager.class.getMethod("setNotificationPolicyAccessGranted", String.class, boolean.class)
                        .invoke(nm, ai.packageName, grant);
                return;
            }
            case "notification_listener": {
                NotificationManager nm = c.getSystemService(NotificationManager.class);
                for (ComponentName cn : components(c, kind).get(ai.packageName))
                    NotificationManager.class.getMethod("setNotificationListenerAccessGranted", ComponentName.class, boolean.class)
                            .invoke(nm, cn, grant);
                return;
            }
            case "accessibility": {
                Set<String> enabled = enabledAccessibility(c);
                for (ComponentName cn : components(c, kind).get(ai.packageName)) {
                    if (grant)
                        enabled.add(cn.flattenToString());
                    else
                        enabled.remove(cn.flattenToString());
                }
                Settings.Secure.putString(c.getContentResolver(), Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
                        String.join(":", enabled));
                Settings.Secure.putInt(c.getContentResolver(), Settings.Secure.ACCESSIBILITY_ENABLED, enabled.isEmpty() ? 0 : 1);
                return;
            }
            case "device_admin": {
                DevicePolicyManager dpm = c.getSystemService(DevicePolicyManager.class);
                for (ComponentName cn : components(c, kind).get(ai.packageName)) {
                    if (grant)
                        DevicePolicyManager.class.getMethod("setActiveAdmin", ComponentName.class, boolean.class)
                                .invoke(dpm, cn, false);
                    else
                        dpm.removeActiveAdmin(cn);
                }
                return;
            }
            default:
                throw new IllegalArgumentException("unknown access");
        }
    }
}
