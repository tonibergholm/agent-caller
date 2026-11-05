import dev.agentcaller.inbox.HostJniAccess;

/** Entry point: java HostJniTest DIR pair|verify|tamper (see jvm-test.sh). */
public class HostJniTest {
    public static void main(String[] args) throws Exception {
        System.exit(HostJniAccess.run(args));
    }
}
