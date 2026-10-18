package android.content.om;

public interface IOverlayManager extends android.os.IInterface {
    void commit(OverlayManagerTransaction transaction) throws android.os.RemoteException;

    abstract class Stub extends android.os.Binder implements IOverlayManager {
        public static IOverlayManager asInterface(android.os.IBinder binder) {
            throw new RuntimeException("stub");
        }
    }
}
