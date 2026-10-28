import java.io.*;
import java.nio.charset.StandardCharsets;

/** Cliente mínimo do protocolo stdin/stdout do AmberLSM. */
public final class AmberClient {
    private static String hex(String value) {
        byte[] bytes = value.getBytes(StandardCharsets.UTF_8);
        StringBuilder result = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) result.append(String.format("%02x", b & 0xff));
        return result.toString();
    }

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("uso: AmberClient <binário> <diretório>");
            System.exit(2);
        }
        Process process = new ProcessBuilder(args[0], args[1])
                .redirectError(ProcessBuilder.Redirect.INHERIT)
                .start();
        try (BufferedWriter input = process.outputWriter(StandardCharsets.UTF_8)) {
            input.write("PUT " + hex("java-client") + " " + hex("mainframe") + "\n");
            input.write("GET " + hex("java-client") + "\n");
            input.write("SCAN\n");
            input.flush();
        }
        try (BufferedReader output = process.inputReader(StandardCharsets.UTF_8)) {
            String line;
            while ((line = output.readLine()) != null) {
                System.out.println(line);
            }
        }
        if (process.waitFor() != 0) throw new IllegalStateException("AmberLSM falhou");
    }
}
