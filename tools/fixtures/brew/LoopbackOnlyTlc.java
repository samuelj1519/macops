import java.io.IOException;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.rmi.server.RMISocketFactory;

public final class LoopbackOnlyTlc extends RMISocketFactory {
    private static InetAddress loopback() throws IOException {
        return InetAddress.getByAddress(new byte[] {127, 0, 0, 1});
    }

    @Override
    public Socket createSocket(String host, int port) throws IOException {
        if (!host.equals("127.0.0.1") && !host.equals("localhost") && !host.equals("::1")) {
            throw new IOException("Non-loopback RMI destination refused");
        }
        return new Socket(loopback(), port);
    }

    @Override
    public ServerSocket createServerSocket(int port) throws IOException {
        return new ServerSocket(port, 50, loopback());
    }

    public static void main(String[] args) throws Exception {
        System.setProperty("java.rmi.server.hostname", "127.0.0.1");
        RMISocketFactory.setSocketFactory(new LoopbackOnlyTlc());
        tlc2.TLC.main(args);
    }
}
