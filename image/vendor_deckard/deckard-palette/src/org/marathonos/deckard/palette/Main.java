package org.marathonos.deckard.palette;

import android.content.om.FabricatedOverlay;
import android.content.om.IOverlayManager;
import android.content.om.OverlayManagerTransaction;
import android.os.ServiceManager;
import android.util.TypedValue;

import java.io.BufferedReader;
import java.io.FileReader;
import java.io.IOException;

/**
 * Applies deckardd's palette (/data/deckard/palette: "system_accent1_100
 * #ff7df0d4" per line) as one fabricated overlay on android, enabled above
 * DeckardPalette in a single transaction, so apps see one configuration
 * change. Runs under app_process as system.
 */
public final class Main {
    private static final String OWNER = "android";
    private static final String NAME = "deckard_ambience";

    public static void main(String[] args) throws Exception {
        String path = args.length > 0 ? args[0] : "/data/deckard/palette";
        FabricatedOverlay.Builder builder = new FabricatedOverlay.Builder(OWNER, NAME, "android");
        int count = 0;
        try (BufferedReader in = new BufferedReader(new FileReader(path))) {
            for (String line; (line = in.readLine()) != null && count < 65; ) {
                String[] f = line.trim().split("\\s+");
                if (f.length != 2 || !f[0].matches("system_(accent[123]|neutral[12])_[0-9]{1,4}")
                        || !f[1].matches("#[0-9a-fA-F]{8}"))
                    continue;
                builder.setResourceValue("android:color/" + f[0], TypedValue.TYPE_INT_COLOR_ARGB8,
                        (int) Long.parseLong(f[1].substring(1), 16));
                count++;
            }
        } catch (IOException e) {
            System.err.println("deckard-palette: " + e.getMessage());
            return;
        }
        if (count == 0)
            return;
        FabricatedOverlay overlay = builder.build();
        IOverlayManager om = IOverlayManager.Stub.asInterface(ServiceManager.getService("overlay"));
        om.commit(new OverlayManagerTransaction.Builder()
                .registerFabricatedOverlay(overlay)
                .setEnabled(overlay.getIdentifier(), true, 0)
                .build());
        System.out.println("deckard-palette: " + count + " colours applied");
    }
}
