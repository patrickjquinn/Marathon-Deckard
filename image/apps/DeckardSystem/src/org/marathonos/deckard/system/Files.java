package org.marathonos.deckard.system;

import android.database.Cursor;
import android.database.MatrixCursor;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.Looper;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.provider.DocumentsContract.Document;
import android.provider.DocumentsContract.Root;
import android.provider.DocumentsProvider;
import android.webkit.MimeTypeMap;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.util.regex.Pattern;

/**
 * Files that crossed from Marathon, or are on their way to it: document
 * "<request id>/<name>" is /data/deckard/xfer/<request id>/<name>, which the
 * host's deckardd fills or empties. It lists no roots, so only the apps it is
 * granted to see it.
 */
public final class Files extends DocumentsProvider {
    static final String AUTHORITY = "org.marathonos.deckard.system.files";
    static final File XFER = new File("/data/deckard/xfer");
    private static final Pattern ID = Pattern.compile("[0-9a-f]{16}");
    private static final String[] DOCUMENT_COLUMNS = {Document.COLUMN_DOCUMENT_ID,
            Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE, Document.COLUMN_SIZE,
            Document.COLUMN_LAST_MODIFIED, Document.COLUMN_FLAGS};

    private final Handler mMain = new Handler(Looper.getMainLooper());

    @Override
    public boolean onCreate() {
        return true;
    }

    static boolean isRequestId(String id) {
        return id != null && ID.matcher(id).matches();
    }

    static File file(String requestId, String name) throws FileNotFoundException {
        if (!isRequestId(requestId) || name == null || name.isEmpty() || name.contains("/")
                || name.equals(".") || name.equals(".."))
            throw new FileNotFoundException(requestId + "/" + name);
        return new File(new File(XFER, requestId), name);
    }

    static String documentId(String requestId, String name) {
        return requestId + "/" + name;
    }

    static android.net.Uri uri(String requestId, String name) {
        return DocumentsContract.buildDocumentUri(AUTHORITY, documentId(requestId, name));
    }

    static String mimeOf(String name) {
        int dot = name.lastIndexOf('.');
        String type = dot < 0 ? null
                : MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substring(dot + 1).toLowerCase());
        return type != null ? type : "application/octet-stream";
    }

    private static File resolve(String documentId) throws FileNotFoundException {
        int slash = documentId.indexOf('/');
        if (slash < 0)
            throw new FileNotFoundException(documentId);
        return file(documentId.substring(0, slash), documentId.substring(slash + 1));
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
                    HostLink.get().send(new JSONObject().put("t", "pick_done").put("id", requestId));
                } catch (JSONException ignored) {
                    // A literal object: cannot happen.
                }
            });
        } catch (IOException e) {
            throw new FileNotFoundException(e.getMessage());
        }
    }
}
