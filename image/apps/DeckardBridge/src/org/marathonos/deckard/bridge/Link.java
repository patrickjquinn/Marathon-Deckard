package org.marathonos.deckard.bridge;

import android.net.LocalSocket;
import android.net.LocalSocketAddress;
import android.os.Handler;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.BufferedInputStream;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;

/** Length-prefixed JSON over deckardd's socket (protocol/bridge.md). */
final class Link {
    interface Listener {
        void onConnected();
        void onMessage(JSONObject message);
    }

    static final String TAG = "DeckardBridge";
    static final String PATH = "/data/deckard/bridge.sock";
    static final int MAX_FRAME = 1 << 20;

    private final Handler mMain;
    private final Listener mListener;
    private final ThreadPoolExecutor mWriter = new ThreadPoolExecutor(1, 1, 0, TimeUnit.SECONDS,
            new LinkedBlockingQueue<>(512), new ThreadPoolExecutor.DiscardPolicy());
    private volatile boolean mRunning = true;
    private LocalSocket mSocket;
    private DataOutputStream mOut;

    Link(Handler main, Listener listener) {
        mMain = main;
        mListener = listener;
        new Thread(this::run, "deckard-link").start();
    }

    void stop() {
        mRunning = false;
        close();
        mWriter.shutdownNow();
    }

    void send(JSONObject message) {
        final byte[] bytes = message.toString().getBytes(StandardCharsets.UTF_8);
        if (bytes.length > MAX_FRAME) {
            Log.w(TAG, "dropped an oversized " + message.optString("t"));
            return;
        }
        mWriter.execute(() -> write(bytes));
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

    private synchronized void close() {
        closeLocked();
    }

    private void closeLocked() {
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
            // Nothing to recover: the socket is gone either way.
        }
        mSocket = null;
        mOut = null;
    }

    private void run() {
        long backoff = 500;
        while (mRunning) {
            LocalSocket socket = new LocalSocket();
            try {
                socket.connect(new LocalSocketAddress(PATH, LocalSocketAddress.Namespace.FILESYSTEM));
                synchronized (this) {
                    mSocket = socket;
                    mOut = new DataOutputStream(socket.getOutputStream());
                }
                backoff = 500;
                Log.i(TAG, "connected to deckardd");
                mMain.post(mListener::onConnected);
                DataInputStream in = new DataInputStream(new BufferedInputStream(socket.getInputStream()));
                while (mRunning) {
                    int length = in.readInt();
                    if (length <= 0 || length > MAX_FRAME)
                        throw new IOException("bad frame length " + length);
                    byte[] bytes = new byte[length];
                    in.readFully(bytes);
                    final JSONObject message = new JSONObject(new String(bytes, StandardCharsets.UTF_8));
                    mMain.post(() -> mListener.onMessage(message));
                }
            } catch (IOException | JSONException e) {
                Log.i(TAG, "link down: " + e.getMessage());
            }
            close();
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
