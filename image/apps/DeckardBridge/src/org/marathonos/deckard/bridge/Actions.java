package org.marathonos.deckard.bridge;

import android.app.SearchManager;
import android.content.ActivityNotFoundException;
import android.content.BroadcastReceiver;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ApplicationInfo;
import android.content.pm.LauncherApps;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.content.pm.ShortcutInfo;
import android.content.res.AssetManager;
import android.content.res.Resources;
import android.content.res.XmlResourceParser;
import android.net.Uri;
import android.os.Handler;
import android.os.Process;
import android.os.UserHandle;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;
import org.xmlpull.v1.XmlPullParser;

import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * Each launchable app as deckardd's Keel Actions see it (protocol/bridge.md,
 * Keel Actions): its links, share and search intents and launcher shortcuts,
 * and the launches Pilot asks for.
 */
final class Actions {
    private static final String NS = "http://schemas.android.com/apk/res/android";
    private static final String GMS_SEARCH = "com.google.android.gms.actions.SEARCH_ACTION";
    private static final int MAX_LINKS = 16;
    private static final int MAX_SHORTCUTS = 15;

    private final Context mContext;
    private final Handler mMain;
    private final Link mLink;
    private final PackageManager mPm;
    private final LauncherApps mLauncher;
    private final ExecutorService mWorker = Executors.newSingleThreadExecutor();
    // Manifest shortcuts' intents, for when Android does not let this app host shortcuts.
    private final Map<String, Map<String, Intent[]>> mStatic = new ConcurrentHashMap<>();
    private final BroadcastReceiver mChanges = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            if (intent.getData() != null)
                resend(intent.getData().getSchemeSpecificPart());
        }
    };
    private final LauncherApps.Callback mShortcutChanges = new LauncherApps.Callback() {
        @Override
        public void onPackageRemoved(String pkg, UserHandle user) {
        }

        @Override
        public void onPackageAdded(String pkg, UserHandle user) {
        }

        @Override
        public void onPackageChanged(String pkg, UserHandle user) {
        }

        @Override
        public void onPackagesAvailable(String[] pkgs, UserHandle user, boolean replacing) {
        }

        @Override
        public void onPackagesUnavailable(String[] pkgs, UserHandle user, boolean replacing) {
        }

        @Override
        public void onShortcutsChanged(String pkg, List<ShortcutInfo> shortcuts, UserHandle user) {
            resend(pkg);
        }
    };

    Actions(Context context, Handler main, Link link) {
        mContext = context;
        mMain = main;
        mLink = link;
        mPm = context.getPackageManager();
        mLauncher = context.getSystemService(LauncherApps.class);
        IntentFilter changes = new IntentFilter();
        changes.addAction(Intent.ACTION_PACKAGE_ADDED);
        changes.addAction(Intent.ACTION_PACKAGE_REMOVED);
        changes.addAction(Intent.ACTION_PACKAGE_CHANGED);
        changes.addDataScheme("package");
        context.registerReceiver(mChanges, changes, null, main);
        if (hostsShortcuts())
            mLauncher.registerCallback(mShortcutChanges, main);
    }

    void stop() {
        mContext.unregisterReceiver(mChanges);
        if (hostsShortcuts())
            mLauncher.unregisterCallback(mShortcutChanges);
        mWorker.shutdownNow();
    }

    private boolean hostsShortcuts() {
        try {
            return mLauncher != null && mLauncher.hasShortcutHostPermission();
        } catch (SecurityException e) {
            return false;
        }
    }

    void handle(String type, JSONObject m) {
        if (type.equals("launch"))
            launch(m);
        else if (type.equals("screen_read"))
            ScreenReader.read(mContext, m, mLink);
        else if (type.equals("ui_act"))
            ScreenReader.act(mContext, m, mLink);
    }

    /** Every app, then the list; deckardd withdraws the ones not listed. */
    void sendAll() {
        mWorker.execute(() -> {
            JSONArray pkgs = new JSONArray();
            for (String pkg : launchable()) {
                JSONObject info = describe(pkg);
                if (info != null) {
                    mLink.send(info);
                    pkgs.put(pkg);
                }
            }
            try {
                mLink.send(new JSONObject().put("t", "app_list").put("pkgs", pkgs));
            } catch (JSONException e) {
                Log.w(Link.TAG, e.getMessage());
            }
        });
    }

    private void resend(String pkg) {
        mWorker.execute(() -> {
            JSONObject info = launchable().contains(pkg) ? describe(pkg) : null;
            try {
                mLink.send(info != null ? info : new JSONObject().put("t", "app_gone").put("pkg", pkg));
            } catch (JSONException e) {
                Log.w(Link.TAG, e.getMessage());
            }
        });
    }

    private Set<String> launchable() {
        Set<String> out = new LinkedHashSet<>();
        Intent main = new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER);
        for (ResolveInfo ri : mPm.queryIntentActivities(main, 0)) {
            String pkg = ri.activityInfo.packageName;
            if (!pkg.startsWith("org.marathonos.deckard."))
                out.add(pkg);
        }
        return out;
    }

    private boolean resolves(Intent intent) {
        return !mPm.queryIntentActivities(intent, PackageManager.MATCH_DEFAULT_ONLY).isEmpty();
    }

    private JSONObject describe(String pkg) {
        try {
            ApplicationInfo ai = mPm.getApplicationInfo(pkg, 0);
            Map<String, String[]> links = new LinkedHashMap<>();
            List<JSONObject> shortcuts = new ArrayList<>();
            Map<String, Intent[]> intents = new LinkedHashMap<>();
            parseManifest(pkg, ai, links, shortcuts, intents);
            if (hostsShortcuts())
                shortcuts = launcherShortcuts(pkg);
            else
                mStatic.put(pkg, intents);

            JSONArray linkList = new JSONArray();
            for (String[] l : links.values())
                linkList.put(new JSONObject().put("s", l[0]).put("h", l[1]));
            JSONArray shortcutList = new JSONArray();
            for (JSONObject s : shortcuts)
                shortcutList.put(s);
            String search = resolves(new Intent(GMS_SEARCH).setPackage(pkg)) ? "gms"
                    : resolves(new Intent(Intent.ACTION_SEARCH).setPackage(pkg)) ? "android" : "";
            return new JSONObject().put("t", "app_info").put("pkg", pkg)
                    .put("label", ai.loadLabel(mPm).toString())
                    .put("links", linkList)
                    .put("share", resolves(new Intent(Intent.ACTION_SEND).setType("text/plain").setPackage(pkg)))
                    .put("search", search)
                    .put("shortcuts", shortcutList);
        } catch (PackageManager.NameNotFoundException | JSONException e) {
            return null;
        }
    }

    private List<JSONObject> launcherShortcuts(String pkg) throws JSONException {
        List<JSONObject> out = new ArrayList<>();
        LauncherApps.ShortcutQuery q = new LauncherApps.ShortcutQuery().setPackage(pkg)
                .setQueryFlags(LauncherApps.ShortcutQuery.FLAG_MATCH_DYNAMIC
                        | LauncherApps.ShortcutQuery.FLAG_MATCH_MANIFEST);
        List<ShortcutInfo> list;
        try {
            list = mLauncher.getShortcuts(q, Process.myUserHandle());
        } catch (SecurityException | IllegalStateException e) {
            return out;
        }
        if (list == null)
            return out;
        list = new ArrayList<>(list);
        Collections.sort(list, (a, b) -> Integer.compare(a.getRank(), b.getRank()));
        for (ShortcutInfo s : list) {
            if (!s.isEnabled() || s.getShortLabel() == null || out.size() >= MAX_SHORTCUTS)
                continue;
            out.add(new JSONObject().put("id", s.getId()).put("label", s.getShortLabel().toString())
                    .put("long", s.getLongLabel() == null ? "" : s.getLongLabel().toString()));
        }
        return out;
    }

    private static String attr(XmlResourceParser p, Resources res, String name) {
        int id = p.getAttributeResourceValue(NS, name, 0);
        if (id != 0) {
            try {
                return res.getString(id);
            } catch (Resources.NotFoundException e) {
                return null;
            }
        }
        return p.getAttributeValue(NS, name);
    }

    // The app's compiled AndroidManifest.xml, among the asset paths of its Resources.
    private static XmlResourceParser openManifest(AssetManager assets, String pkg) {
        for (int cookie = 1; cookie < 16; cookie++) {
            try {
                XmlResourceParser p = assets.openXmlResourceParser(cookie, "AndroidManifest.xml");
                int e;
                while ((e = p.next()) != XmlPullParser.START_TAG && e != XmlPullParser.END_DOCUMENT) {
                }
                if (e == XmlPullParser.START_TAG && pkg.equals(p.getAttributeValue(null, "package"))) {
                    p.close();
                    return assets.openXmlResourceParser(cookie, "AndroidManifest.xml");
                }
                p.close();
            } catch (Exception e) {
                // No such cookie, or not a manifest.
            }
        }
        return null;
    }

    private void parseManifest(String pkg, ApplicationInfo ai, Map<String, String[]> links,
            List<JSONObject> shortcuts, Map<String, Intent[]> intents) {
        Resources res;
        try {
            res = mPm.getResourcesForApplication(ai);
        } catch (PackageManager.NameNotFoundException e) {
            return;
        }
        XmlResourceParser p = openManifest(res.getAssets(), pkg);
        if (p == null)
            return;
        boolean inActivity = false, usable = false, inFilter = false, launcher = false;
        int activityShortcuts = 0, shortcutsXml = 0;
        Set<String> actions = new HashSet<>(), categories = new HashSet<>();
        List<String> schemes = new ArrayList<>(), hosts = new ArrayList<>();
        try {
            for (int e = p.next(); e != XmlPullParser.END_DOCUMENT; e = p.next()) {
                String tag = p.getName();
                if (e == XmlPullParser.START_TAG) {
                    if (tag.equals("activity") || tag.equals("activity-alias")) {
                        inActivity = true;
                        usable = p.getAttributeBooleanValue(NS, "exported", true)
                                && p.getAttributeBooleanValue(NS, "enabled", true);
                        launcher = false;
                        activityShortcuts = 0;
                    } else if (inActivity && tag.equals("intent-filter")) {
                        inFilter = true;
                        actions.clear();
                        categories.clear();
                        schemes.clear();
                        hosts.clear();
                    } else if (inFilter && tag.equals("action")) {
                        actions.add(attr(p, res, "name"));
                    } else if (inFilter && tag.equals("category")) {
                        categories.add(attr(p, res, "name"));
                    } else if (inFilter && tag.equals("data")) {
                        String s = attr(p, res, "scheme");
                        String h = attr(p, res, "host");
                        if (s != null)
                            schemes.add(s.toLowerCase());
                        if (h != null)
                            hosts.add(h.toLowerCase());
                    } else if (inActivity && tag.equals("meta-data")
                            && "android.app.shortcuts".equals(attr(p, res, "name"))) {
                        activityShortcuts = p.getAttributeResourceValue(NS, "resource", 0);
                    }
                } else if (e == XmlPullParser.END_TAG) {
                    if (tag.equals("intent-filter") && inFilter) {
                        inFilter = false;
                        if (actions.contains(Intent.ACTION_MAIN) && categories.contains(Intent.CATEGORY_LAUNCHER))
                            launcher = true;
                        if (usable && actions.contains(Intent.ACTION_VIEW)
                                && categories.contains(Intent.CATEGORY_BROWSABLE)) {
                            for (String s : schemes) {
                                for (String h : hosts.isEmpty() ? Collections.singletonList("") : hosts) {
                                    if (links.size() < MAX_LINKS)
                                        links.put(s + ":" + h, new String[] {s, h});
                                }
                            }
                        }
                    } else if (tag.equals("activity") || tag.equals("activity-alias")) {
                        if (launcher && activityShortcuts != 0 && shortcutsXml == 0)
                            shortcutsXml = activityShortcuts;
                        inActivity = false;
                    }
                }
            }
        } catch (Exception e) {
            Log.w(Link.TAG, "manifest of " + pkg + ": " + e.getMessage());
        } finally {
            p.close();
        }
        if (shortcutsXml != 0)
            parseShortcuts(pkg, res, shortcutsXml, shortcuts, intents);
    }

    private static void parseShortcuts(String pkg, Resources res, int xml, List<JSONObject> shortcuts,
            Map<String, Intent[]> intents) {
        String id = null, label = null, longLabel = null;
        boolean enabled = true;
        List<Intent> chain = new ArrayList<>();
        Intent current = null;
        try (XmlResourceParser p = res.getXml(xml)) {
            for (int e = p.next(); e != XmlPullParser.END_DOCUMENT; e = p.next()) {
                String tag = p.getName();
                if (e == XmlPullParser.START_TAG && tag.equals("shortcut")) {
                    id = attr(p, res, "shortcutId");
                    label = attr(p, res, "shortcutShortLabel");
                    longLabel = attr(p, res, "shortcutLongLabel");
                    enabled = p.getAttributeBooleanValue(NS, "enabled", true);
                    chain.clear();
                } else if (e == XmlPullParser.START_TAG && tag.equals("intent")) {
                    current = new Intent(attr(p, res, "action"));
                    String target = attr(p, res, "targetClass");
                    String targetPkg = attr(p, res, "targetPackage");
                    if (target != null)
                        current.setComponent(new ComponentName(targetPkg != null ? targetPkg : pkg, target));
                    else if (targetPkg != null)
                        current.setPackage(targetPkg);
                    String data = attr(p, res, "data");
                    if (data != null)
                        current.setData(Uri.parse(data));
                    chain.add(current);
                } else if (e == XmlPullParser.START_TAG && tag.equals("categories") && current != null) {
                    current.addCategory(attr(p, res, "name"));
                } else if (e == XmlPullParser.START_TAG && tag.equals("extra") && current != null) {
                    current.putExtra(attr(p, res, "name"), attr(p, res, "value"));
                } else if (e == XmlPullParser.END_TAG && tag.equals("intent")) {
                    current = null;
                } else if (e == XmlPullParser.END_TAG && tag.equals("shortcut")) {
                    if (id != null && label != null && enabled && !chain.isEmpty()
                            && shortcuts.size() < MAX_SHORTCUTS) {
                        Intent[] all = chain.toArray(new Intent[0]);
                        all[0].addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK);
                        intents.put(id, all);
                        shortcuts.add(new JSONObject().put("id", id).put("label", label)
                                .put("long", longLabel == null ? "" : longLabel));
                    }
                    id = null;
                }
            }
        } catch (Exception e) {
            Log.w(Link.TAG, "shortcuts of " + pkg + ": " + e.getMessage());
        }
    }

    private void launch(JSONObject m) {
        String pkg = m.optString("pkg");
        String kind = m.optString("kind");
        String error = null;
        try {
            if (kind.equals("shortcut")) {
                String id = m.optString("id");
                if (hostsShortcuts()) {
                    mLauncher.startShortcut(pkg, id, null, null, Process.myUserHandle());
                } else {
                    Map<String, Intent[]> known = mStatic.get(pkg);
                    Intent[] chain = known == null ? null : known.get(id);
                    if (chain == null)
                        error = "Android does not offer that shortcut here";
                    else
                        mContext.startActivities(chain);
                }
            } else {
                Intent i;
                if (kind.equals("view")) {
                    i = new Intent(Intent.ACTION_VIEW, Uri.parse(m.optString("uri")))
                            .addCategory(Intent.CATEGORY_BROWSABLE);
                } else if (kind.equals("send")) {
                    i = new Intent(Intent.ACTION_SEND).setType("text/plain")
                            .putExtra(Intent.EXTRA_TEXT, m.optString("text"));
                    if (m.has("subject"))
                        i.putExtra(Intent.EXTRA_SUBJECT, m.optString("subject"));
                } else if (kind.equals("search")) {
                    i = new Intent("gms".equals(m.optString("mode")) ? GMS_SEARCH : Intent.ACTION_SEARCH)
                            .putExtra(SearchManager.QUERY, m.optString("query"));
                } else {
                    throw new ActivityNotFoundException("unknown kind " + kind);
                }
                i.setPackage(pkg).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                if (mPm.resolveActivity(i, PackageManager.MATCH_DEFAULT_ONLY) == null)
                    error = "the app does not open that";
                else
                    mContext.startActivity(i);
            }
        } catch (ActivityNotFoundException | SecurityException | IllegalStateException
                | IllegalArgumentException e) {
            error = e.getMessage() != null ? e.getMessage() : e.getClass().getSimpleName();
        }
        try {
            JSONObject out = new JSONObject().put("t", "launched").put("req", m.optLong("req"))
                    .put("ok", error == null);
            if (error != null)
                out.put("error", error);
            mLink.send(out);
        } catch (JSONException e) {
            Log.w(Link.TAG, e.getMessage());
        }
    }
}
