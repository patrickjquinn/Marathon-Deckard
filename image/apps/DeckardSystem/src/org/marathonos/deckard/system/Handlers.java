package org.marathonos.deckard.system;

import android.content.BroadcastReceiver;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.net.Uri;
import android.util.Log;

import java.util.List;

/**
 * Makes Forward the preferred activity for the schemes it hands to Marathon,
 * as the browser role does for http and https, so a second Android handler
 * never brings up Android's resolver. Android drops a preference whose set of
 * handlers changed, so it is set again on every package change.
 */
final class Handlers {
    private static final String TAG = "DeckardHandlers";
    /** Forward's intent filters, as action and scheme. */
    private static final String[][] HANDLED = {
        {Intent.ACTION_VIEW, "geo"}, {Intent.ACTION_SENDTO, "geo"},
        {Intent.ACTION_VIEW, "tel"}, {Intent.ACTION_DIAL, "tel"},
        {Intent.ACTION_VIEW, "sms"}, {Intent.ACTION_SENDTO, "sms"},
        {Intent.ACTION_VIEW, "smsto"}, {Intent.ACTION_SENDTO, "smsto"},
        {Intent.ACTION_VIEW, "mms"}, {Intent.ACTION_SENDTO, "mms"},
        {Intent.ACTION_VIEW, "mmsto"}, {Intent.ACTION_SENDTO, "mmsto"},
        {Intent.ACTION_VIEW, "mailto"}, {Intent.ACTION_SENDTO, "mailto"},
    };

    private Handlers() {}

    static void start(Context c) {
        IntentFilter packages = new IntentFilter();
        packages.addAction(Intent.ACTION_PACKAGE_ADDED);
        packages.addAction(Intent.ACTION_PACKAGE_CHANGED);
        packages.addAction(Intent.ACTION_PACKAGE_REMOVED);
        packages.addDataScheme("package");
        c.registerReceiver(new BroadcastReceiver() {
            @Override
            public void onReceive(Context context, Intent intent) {
                prefer(context);
            }
        }, packages, Context.RECEIVER_EXPORTED);
        prefer(c);
    }

    @SuppressWarnings("deprecation")
    private static void prefer(Context c) {
        PackageManager pm = c.getPackageManager();
        ComponentName forward = new ComponentName(c, Forward.class);
        try {
            pm.clearPackagePreferredActivities(c.getPackageName());
            for (String[] handled : HANDLED) {
                String action = handled[0];
                String scheme = handled[1];
                Intent probe = new Intent(action, Uri.fromParts(scheme, "0", null));
                List<ResolveInfo> handlers = pm.queryIntentActivities(probe, PackageManager.MATCH_DEFAULT_ONLY);
                if (handlers.size() < 2)
                    continue;
                ComponentName[] set = new ComponentName[handlers.size()];
                int match = 0;
                boolean ours = false;
                for (int i = 0; i < set.length; i++) {
                    ResolveInfo r = handlers.get(i);
                    set[i] = new ComponentName(r.activityInfo.packageName, r.activityInfo.name);
                    match = Math.max(match, r.match & IntentFilter.MATCH_CATEGORY_MASK);
                    ours |= forward.equals(set[i]);
                }
                if (!ours)
                    continue;
                IntentFilter filter = new IntentFilter(action);
                filter.addCategory(Intent.CATEGORY_DEFAULT);
                filter.addCategory(Intent.CATEGORY_BROWSABLE);
                filter.addDataScheme(scheme);
                pm.addPreferredActivity(filter, match, set, forward);
                Log.i(TAG, action + " " + scheme + ": Marathon over " + (set.length - 1) + " other handler(s)");
            }
        } catch (RuntimeException e) {
            Log.w(TAG, "preferred handlers not set", e);
        }
    }
}
