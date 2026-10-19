package org.marathonos.deckard.system;

import android.net.LocalSocket;
import android.net.LocalSocketAddress;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.BufferedInputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;

/** The system channel to deckardd: length-prefixed JSON (protocol/bridge.md). */
final class HostLink {
    interface Reply {
        void onReply(JSONObject message);
    }

    static final String TAG = "DeckardSystem";
    static final String PATH = "/data/deckard/system.sock";
    static final int MAX_FRAME = 1 << 20;

    private static HostLink sInstance;

    private final Handler mMain = new Handler(Looper.getMainLooper());
    private final ThreadPoolExecutor mWriter = new ThreadPoolExecutor(1, 1, 0, TimeUnit.SECONDS,
            new LinkedBlockingQueue<>(256), new ThreadPoolExecutor.DiscardPolicy());
    private final Map<String, Reply> mWaiting = new HashMap<>();
    private volatile boolean mConnected;
    private LocalSocket mSocket;
    private DataOutputStream mOut;

    static synchronized HostLink get() {
        if (sInstance == null)
            sInstance = new HostLink();
        return sInstance;
    }

    private HostLink() {
        new Thread(this::run, "deckard-system-link").start();
    }

    boolean isConnected() {
        return mConnected;
    }

    void send(JSONObject message) {
        final byte[] bytes = message.toString().getBytes(StandardCharsets.UTF_8);
        if (bytes.length > MAX_FRAME)
            return;
        mWriter.execute(() -> write(bytes));
    }

    /** Sends a request and calls reply on the main thread with the host's answer, or null. */
    void request(String id, JSONObject message, Reply reply) {
        mWaiting.put(id, reply);
        if (!mConnected) {
            finish(id, null);
            return;
        }
        send(message);
    }

    void forget(String id) {
        mWaiting.remove(id);
    }

    private void finish(String id, JSONObject message) {
        Reply reply = mWaiting.remove(id);
        if (reply != null)
            reply.onReply(message);
    }

    private synchronized void write(byte[] bytes) {
        if (mOut == null)
            return;
        try {
            mOut.writeInt(bytes.length);
            mOut.write(bytes);
            mOut.flush();
        } catch (IOException e) {
            closeLocked();
        }
    }

    private synchronized void closeLocked() {
        if (mSocket == null)
            return;
        try {
            mSocket.shutdownInput();
        } catch (IOException ignored) {
            // Already shut down by the peer.
        }
        try {
            mSocket.close();
        } catch (IOException ignored) {
            // Gone either way.
        }
        mSocket = null;
        mOut = null;
    }

    private void onMessage(JSONObject m) {
        if ("picked".equals(m.optString("t")))
            finish(m.optString("id"), m);
        else if ("manage".equals(m.optString("t")))
            Manage.handle(m);
        else if ("keel".equals(m.optString("t")))
            AppActions.handle(m);
        else if ("focus".equals(m.optString("t")))
            WindowFit.focus(m.optString("pkg"));
    }

    private void onDisconnected() {
        for (String id : mWaiting.keySet().toArray(new String[0]))
            finish(id, null);
    }

    private void run() {
        long backoff = 500;
        while (true) {
            LocalSocket socket = new LocalSocket();
            try {
                socket.connect(new LocalSocketAddress(PATH, LocalSocketAddress.Namespace.FILESYSTEM));
                synchronized (this) {
                    mSocket = socket;
                    mOut = new DataOutputStream(socket.getOutputStream());
                }
                backoff = 500;
                mConnected = true;
                Log.i(TAG, "connected to deckardd");
                send(new JSONObject().put("t", "hello").put("v", 1));
                DataInputStream in = new DataInputStream(new BufferedInputStream(socket.getInputStream()));
                while (true) {
                    int length = in.readInt();
                    if (length <= 0 || length > MAX_FRAME)
                        throw new IOException("bad frame length " + length);
                    byte[] bytes = new byte[length];
                    in.readFully(bytes);
                    final JSONObject message = new JSONObject(new String(bytes, StandardCharsets.UTF_8));
                    mMain.post(() -> onMessage(message));
                }
            } catch (IOException | JSONException e) {
                if (mConnected)
                    Log.i(TAG, "link down: " + e.getMessage());
            }
            mConnected = false;
            synchronized (this) {
                closeLocked();
            }
            mMain.post(this::onDisconnected);
            try {
                socket.close();
            } catch (IOException ignored) {
                // Never connected.
            }
            try {
                Thread.sleep(backoff);
            } catch (InterruptedException e) {
                return;
            }
            backoff = Math.min(backoff * 2, 10000);
        }
    }
}
