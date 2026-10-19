package org.marathonos.deckard.bridge;

import android.app.Service;
import android.content.Intent;
import android.location.Location;
import android.location.LocationManager;
import android.os.IBinder;
import android.os.SystemClock;
import android.os.WorkSource;
import android.util.Log;

import com.android.location.provider.LocationProviderBase;
import com.android.location.provider.ProviderPropertiesUnbundled;
import com.android.location.provider.ProviderRequestUnbundled;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * Android's network location provider, fed by Marathon's location (GeoClue
 * through deckardd, protocol/bridge.md). Android binds it for as long as it
 * runs and says when apps want locations; deckardd only locates then.
 */
public final class NetworkLocation extends Service {
    private static final int POWER_USAGE_LOW = 1;
    private static final int ACCURACY_FINE = 1;
    private static volatile Provider sProvider;

    @Override
    public void onCreate() {
        sProvider = new Provider(this);
    }

    @Override
    public IBinder onBind(Intent intent) {
        return sProvider.getBinder();
    }

    @Override
    public void onDestroy() {
        Provider p = sProvider;
        sProvider = null;
        if (p != null)
            p.request(false, 0);
    }

    /** The bridge channel came up: tell deckardd what Android wants. */
    static void onConnected() {
        Provider p = sProvider;
        if (p != null)
            p.send();
    }

    static void onLocation(JSONObject m) {
        Provider p = sProvider;
        if (p == null || !p.mOn)
            return;
        Location l = new Location(LocationManager.NETWORK_PROVIDER);
        l.setLatitude(m.optDouble("lat"));
        l.setLongitude(m.optDouble("lon"));
        l.setAccuracy((float) m.optDouble("acc"));
        if (m.has("alt"))
            l.setAltitude(m.optDouble("alt"));
        if (m.has("speed"))
            l.setSpeed((float) m.optDouble("speed"));
        if (m.has("heading"))
            l.setBearing((float) m.optDouble("heading"));
        long time = m.optLong("time");
        long age = Math.max(0, System.currentTimeMillis() - time);
        l.setTime(time);
        l.setElapsedRealtimeNanos(SystemClock.elapsedRealtimeNanos() - age * 1_000_000L);
        if (!l.isComplete() || Double.isNaN(l.getLatitude()) || Double.isNaN(l.getLongitude()))
            return;
        p.reportLocation(l);
    }

    private static final class Provider extends LocationProviderBase {
        volatile boolean mOn;
        private long mInterval;

        Provider(NetworkLocation service) {
            super(service, Link.TAG, ProviderPropertiesUnbundled.create(true, false, false, false,
                    true, true, true, POWER_USAGE_LOW, ACCURACY_FINE));
        }

        @Override
        protected void onSetRequest(ProviderRequestUnbundled request, WorkSource source) {
            request(request.getReportLocation(), request.getInterval());
        }

        synchronized void request(boolean on, long interval) {
            if (on == mOn && (!on || interval == mInterval))
                return;
            mOn = on;
            mInterval = interval;
            send();
        }

        synchronized void send() {
            try {
                BridgeService.send(new JSONObject().put("t", "location_request").put("on", mOn)
                        .put("interval", mOn ? mInterval : 0));
            } catch (JSONException e) {
                Log.w(Link.TAG, e.getMessage());
            }
        }
    }
}
