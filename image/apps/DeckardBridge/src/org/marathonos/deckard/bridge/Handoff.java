package org.marathonos.deckard.bridge;

import android.content.Intent;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.Binder;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.os.Process;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;
import android.util.Log;
import android.webkit.MimeTypeMap;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.util.ArrayList;
import java.util.regex.Pattern;

/**
 * Files that crossed from Marathon, or are on their way to it: document
 * "<request id>/<name>" is /data/deckard/xfer/<request id>/<name>, which
 * deckardd fills or empties. DeckardSystem, which hands these files to
 * apps, runs as the system uid, and Android takes no URI grant from the
 * system uid; so the provider lives here and DeckardSystem asks it (call
 * "grant") to grant the app access. It lists no roots. DeckardSystem's
 * text toasts come through here too (call "toast").
 */
public final class Handoff extends DocumentsProvider {
    static final String AUTHORITY = "org.marathonos.deckard.bridge.files";
    private static final File XFER = new File("/data/deckard/xfer");
    private static final Pattern ID = Pattern.compile("[0-9a-f]{16}");
    private static final String[] DOCUMENT_COLUMNS = {Document.COLUMN_DOCUMENT_ID,
            Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE, Document.COLUMN_SIZE,
            Document.COLUMN_LAST_MODIFIED, Document.COLUMN_FLAGS};

    private final Handler mMain = new Handler(Looper.getMainLooper());

    @Override
    public boolean onCreate() {
        return true;
    }

    private static File resolve(String documentId) throws FileNotFoundException {
        int slash = documentId == null ? -1 : documentId.indexOf('/');
        if (slash < 0)
            throw new FileNotFoundException(documentId);
        String id = documentId.substring(0, slash);
        String name = documentId.substring(slash + 1);
        if (!ID.matcher(id).matches() || name.isEmpty() || name.contains("/") || name.equals(".")
                || name.equals(".."))
            throw new FileNotFoundException(documentId);
        return new File(new File(XFER, id), name);
    }

    static String mimeOf(String name) {
        int dot = name.lastIndexOf('.');
        String type = dot < 0 ? null
                : MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase());
        return type != null ? type : "application/octet-stream";
    }

    @Override
    public Bundle call(String method, String arg, Bundle extras) {
        if (!"grant".equals(method) && !"toast".equals(method))
            return super.call(method, arg, extras);
        if (Binder.getCallingUid() != Process.SYSTEM_UID || extras == null)
            throw new SecurityException(method + " is for DeckardSystem");
        String pkg = extras.getString("pkg", "");
        if ("toast".equals(method)) {
            BridgeService.toast(pkg, extras.getString("text", ""));
            return null;
        }
        int flags = Intent.FLAG_GRANT_READ_URI_PERMISSION
                | (extras.getBoolean("write") ? Intent.FLAG_GRANT_WRITE_URI_PERMISSION : 0)
                | (extras.getBoolean("persist") ? Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION : 0);
        ArrayList<Uri> uris = extras.getParcelableArrayList("uris", Uri.class);
        long token = Binder.clearCallingIdentity();
        try {
            for (Uri uri : uris == null ? new ArrayList<Uri>() : uris) {
                if (AUTHORITY.equals(uri.getAuthority()))
                    getContext().grantUriPermission(pkg, uri, flags);
            }
        } finally {
            Binder.restoreCallingIdentity(token);
        }
        return null;
    }

    @Override
    public Cursor queryRoots(String[] projection) {
        return new MatrixCursor(projection != null ? projection : new String[] {Root.COLUMN_ROOT_ID});
    }

    @Override
    public Cursor queryDocument(String documentId, String[] projection) throws FileNotFoundException {
        File f = resolve(documentId);
        if (!f.isFile())
            throw new FileNotFoundException(documentId);
        MatrixCursor c = new MatrixCursor(projection != null ? projection : DOCUMENT_COLUMNS);
        MatrixCursor.RowBuilder row = c.newRow();
        row.add(Document.COLUMN_DOCUMENT_ID, documentId);
        row.add(Document.COLUMN_DISPLAY_NAME, f.getName());
        row.add(Document.COLUMN_MIME_TYPE, mimeOf(f.getName()));
        row.add(Document.COLUMN_SIZE, f.length());
        row.add(Document.COLUMN_LAST_MODIFIED, f.lastModified());
        row.add(Document.COLUMN_FLAGS, Document.FLAG_SUPPORTS_WRITE);
        return c;
    }

    @Override
    public Cursor queryChildDocuments(String parentDocumentId, String[] projection, String sortOrder)
            throws FileNotFoundException {
        throw new FileNotFoundException(parentDocumentId);
    }

    @Override
    public String getDocumentType(String documentId) throws FileNotFoundException {
        return mimeOf(resolve(documentId).getName());
    }

    @Override
    public ParcelFileDescriptor openDocument(String documentId, String mode, CancellationSignal signal)
            throws FileNotFoundException {
        File f = resolve(documentId);
        int flags = ParcelFileDescriptor.parseMode(mode);
        if ((flags & ParcelFileDescriptor.MODE_WRITE_ONLY) == 0
                && (flags & ParcelFileDescriptor.MODE_READ_WRITE) == 0)
            return ParcelFileDescriptor.open(f, flags);
        final String requestId = documentId.substring(0, documentId.indexOf('/'));
        try {
            // Each close of a written file tells deckardd to copy it to where the person chose.
            return ParcelFileDescriptor.open(f, flags, mMain, e -> {
                try {
                    BridgeService.send(new JSONObject().put("t", "pick_done").put("id", requestId));
                } catch (JSONException ignored) {
                    // A literal object: cannot happen.
                }
            });
        } catch (IOException e) {
            Log.w(Link.TAG, "open " + documentId + ": " + e);
            throw new FileNotFoundException(e.getMessage());
        }
    }
}
