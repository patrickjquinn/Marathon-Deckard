package com.android.location.provider;

import android.content.Context;
import android.location.Location;
import android.os.IBinder;
import android.os.WorkSource;

/** Compile-time stub of the com.android.location.provider library (AOSP 13). */
public abstract class LocationProviderBase {
    public LocationProviderBase(Context context, String tag, ProviderPropertiesUnbundled properties) {
        throw new RuntimeException("stub");
    }

    public IBinder getBinder() {
        throw new RuntimeException("stub");
    }

    public void setAllowed(boolean allowed) {
        throw new RuntimeException("stub");
    }

    public void reportLocation(Location location) {
        throw new RuntimeException("stub");
    }

    protected abstract void onSetRequest(ProviderRequestUnbundled request, WorkSource source);
}
