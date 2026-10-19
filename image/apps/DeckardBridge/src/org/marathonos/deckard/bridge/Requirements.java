package org.marathonos.deckard.bridge;

import android.content.pm.FeatureInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.res.Resources;
import android.content.res.XmlResourceParser;
import android.os.Build;
import android.os.Bundle;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;
import org.xmlpull.v1.XmlPullParser;

import java.io.File;
import java.io.IOException;
import java.util.Enumeration;
import java.util.TreeSet;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

/**
 * What an app needs from the phone, from its .apk, and what this phone has
 * (protocol/bridge.md, compatibility): required features and libraries,
 * Google Play services, native code. The Store compares the two.
 */
final class Requirements {
    private static final String ANDROID = "http://schemas.android.com/apk/res/android";
    private static final String GMS = "com.google.android.gms";
    private static final int MAX = 64;

    private Requirements() {}

    /** Adds features, libraries, gms and abis for an .apk parsed with GET_CONFIGURATIONS and GET_META_DATA. */
    static void of(PackageManager pm, PackageInfo info, File apk, JSONObject out) throws JSONException {
        TreeSet<String> features = new TreeSet<>();
        if (info.reqFeatures != null) {
            for (FeatureInfo f : info.reqFeatures) {
                if (f.name != null && (f.flags & FeatureInfo.FLAG_REQUIRED) != 0)
                    features.add(f.name);
            }
        }
        TreeSet<String> libraries = new TreeSet<>();
        boolean gms = false;
        Bundle meta = info.applicationInfo.metaData;
        if (meta != null && meta.containsKey(GMS + ".version"))
            gms = true;
        try {
            Resources res = pm.getResourcesForApplication(info.applicationInfo);
            gms |= libraries(res, info.packageName, libraries);
        } catch (PackageManager.NameNotFoundException | RuntimeException e) {
            // No manifest to read: the libraries stay unknown.
        }
        out.put("features", array(features)).put("libraries", array(libraries)).put("gms", gms)
                .put("abis", array(abis(apk)));
    }

    /** This phone: its features, shared libraries, ABIs, SDK and whether Play services are there. */
    static JSONObject device(PackageManager pm) throws JSONException {
        TreeSet<String> features = new TreeSet<>();
        for (FeatureInfo f : pm.getSystemAvailableFeatures()) {
            if (f.name != null)
                features.add(f.name);
        }
        TreeSet<String> libraries = new TreeSet<>();
        String[] names = pm.getSystemSharedLibraryNames();
        if (names != null)
            java.util.Collections.addAll(libraries, names);
        // Play services or microG, installed and not turned off in Settings.
        boolean gms;
        try {
            gms = pm.getApplicationInfo(GMS,
                    PackageManager.ApplicationInfoFlags.of(PackageManager.MATCH_DISABLED_COMPONENTS)).enabled;
        } catch (PackageManager.NameNotFoundException e) {
            gms = false;
        }
        JSONArray abis = new JSONArray();
        for (String abi : Build.SUPPORTED_ABIS)
            abis.put(abi);
        return new JSONObject().put("features", new JSONArray(features)).put("libraries", new JSONArray(libraries))
                .put("abis", abis).put("sdk", Build.VERSION.SDK_INT).put("gms", gms);
    }

    // The app's own AndroidManifest.xml is one of its resources' asset
    // cookies; the framework's is another, so the package name tells them
    // apart. Returns whether it uses Play services' library.
    private static boolean libraries(Resources res, String pkg, TreeSet<String> out) {
        for (int cookie = 1; cookie <= 8; cookie++) {
            try (XmlResourceParser xp = res.getAssets().openXmlResourceParser(cookie, "AndroidManifest.xml")) {
                int e = xp.next();
                while (e != XmlPullParser.START_TAG && e != XmlPullParser.END_DOCUMENT)
                    e = xp.next();
                if (e != XmlPullParser.START_TAG || !"manifest".equals(xp.getName())
                        || !pkg.equals(xp.getAttributeValue(null, "package")))
                    continue;
                boolean gms = false;
                for (e = xp.next(); e != XmlPullParser.END_DOCUMENT; e = xp.next()) {
                    if (e != XmlPullParser.START_TAG || !"uses-library".equals(xp.getName())
                            || !xp.getAttributeBooleanValue(ANDROID, "required", true))
                        continue;
                    String name = xp.getAttributeValue(ANDROID, "name");
                    if (name == null)
                        continue;
                    if (out.size() < MAX)
                        out.add(name);
                    gms |= name.startsWith(GMS);
                }
                return gms;
            } catch (IOException | org.xmlpull.v1.XmlPullParserException | RuntimeException ex) {
                // No such cookie, or not a manifest.
            }
        }
        return false;
    }

    private static TreeSet<String> abis(File apk) {
        TreeSet<String> out = new TreeSet<>();
        try (ZipFile zip = new ZipFile(apk)) {
            Enumeration<? extends ZipEntry> entries = zip.entries();
            while (entries.hasMoreElements() && out.size() < 16) {
                String name = entries.nextElement().getName();
                if (!name.startsWith("lib/") || !name.endsWith(".so"))
                    continue;
                int slash = name.indexOf('/', 4);
                if (slash > 4)
                    out.add(name.substring(4, slash));
            }
        } catch (IOException e) {
            // Unreadable: no native code known.
        }
        return out;
    }

    private static JSONArray array(TreeSet<String> set) {
        return new JSONArray(set);
    }
}
