package org.marathonos.deckard.system;

import android.content.Context;
import android.net.Uri;
import android.os.Bundle;
import android.provider.DocumentsContract;

import java.io.File;
import java.io.FileNotFoundException;
import java.util.ArrayList;
import java.util.List;
import java.util.regex.Pattern;

/**
 * Files that crossed from Marathon, or are on their way to it:
 * /data/deckard/xfer/<request id>/<name>. Apps reach them through
 * DeckardBridge's Handoff provider, which grants them access when asked:
 * Android takes no URI grant from this app's system uid.
 */
final class Files {
    static final String AUTHORITY = "org.marathonos.deckard.bridge.files";
    static final File XFER = new File("/data/deckard/xfer");
    private static final Pattern ID = Pattern.compile("[0-9a-f]{16}");

    private Files() {}

    static boolean isRequestId(String id) {
        return id != null && ID.matcher(id).matches();
    }

    static File file(String requestId, String name) throws FileNotFoundException {
        if (!isRequestId(requestId) || name == null || name.isEmpty() || name.contains("/")
                || name.equals(".") || name.equals(".."))
            throw new FileNotFoundException(requestId + "/" + name);
        return new File(new File(XFER, requestId), name);
    }

    static Uri uri(String requestId, String name) {
        return DocumentsContract.buildDocumentUri(AUTHORITY, requestId + "/" + name);
    }

    /**
     * Lets pkg open the request's files through Handoff. deckardd's copies
     * keep the modes of their originals and belong to this uid, so they are
     * opened up for DeckardBridge's uid first; a file to save into must be
     * writable by it too.
     */
    static void grant(Context c, String pkg, String requestId, List<String> names, boolean write, boolean persist)
            throws FileNotFoundException {
        ArrayList<Uri> uris = new ArrayList<>();
        for (String name : names) {
            File f = file(requestId, name);
            File dir = f.getParentFile();
            dir.setReadable(true, false);
            dir.setExecutable(true, false);
            f.setReadable(true, false);
            if (write)
                f.setWritable(true, false);
            uris.add(uri(requestId, name));
        }
        if (pkg == null || pkg.isEmpty() || uris.isEmpty())
            return;
        Bundle extras = new Bundle();
        extras.putString("pkg", pkg);
        extras.putParcelableArrayList("uris", uris);
        extras.putBoolean("write", write);
        extras.putBoolean("persist", persist);
        c.getContentResolver().call(AUTHORITY, "grant", null, extras);
    }
}
