import java.io.ByteArrayOutputStream;
import java.io.Externalizable;
import java.io.IOException;
import java.io.ObjectInput;
import java.io.ObjectInputStream;
import java.io.ObjectOutput;
import java.io.ObjectOutputStream;
import java.io.ObjectStreamConstants;
import java.io.Serial;
import java.io.Serializable;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.util.HashMap;
import java.util.HexFormat;

/**
 * Generates the byte fixtures used by the Java serialization codec tests.
 *
 * <p>The fixtures deliberately come from JDK 21's ObjectOutputStream instead of being assembled
 * from our parser's interpretation of the wire format. Run without creating class files:
 *
 * <pre>java scripts/dev/GenerateJavaCodecFixtures.java</pre>
 */
public final class GenerateJavaCodecFixtures {
    private GenerateJavaCodecFixtures() {}

    @FunctionalInterface
    private interface StreamWriter {
        void write(ObjectOutputStream output) throws IOException;
    }

    private static byte[] serialize(StreamWriter writer) throws IOException {
        return serialize(ObjectStreamConstants.PROTOCOL_VERSION_2, writer);
    }

    private static byte[] serialize(int protocol, StreamWriter writer) throws IOException {
        var bytes = new ByteArrayOutputStream();
        try (var output = new ObjectOutputStream(bytes)) {
            output.useProtocolVersion(protocol);
            writer.write(output);
        }
        return bytes.toByteArray();
    }

    private static void print(String name, byte[] bytes) {
        System.out.printf("%s=%s%n", name, HexFormat.of().formatHex(bytes));
    }

    private static byte[] fatalExceptionStream() throws IOException {
        var bytes = new ByteArrayOutputStream();
        try (var output = new ObjectOutputStream(bytes)) {
            try {
                output.writeObject(new FailingRecord());
                throw new AssertionError("fixture object should have failed while writing");
            } catch (IOException expected) {
                if (!"fixture failure".equals(expected.getMessage())) throw expected;
            }
        }
        return bytes.toByteArray();
    }

    public static void main(String[] args) throws IOException {
        print("modifiedUtfString", serialize(output -> output.writeObject("A\u0000\u00e9\ud83d\ude00")));
        print("primitiveArray", serialize(output -> output.writeObject(new int[] {-1, 0, 42, Integer.MAX_VALUE})));
        print("objectArray", serialize(output -> output.writeObject(new String[] {"left", null, "right"})));
        print("inheritance", serialize(output -> output.writeObject(new ChildRecord())));

        var map = new HashMap<String, Integer>();
        map.put("alpha", 1);
        map.put("beta", 2);
        print("hashMap", serialize(output -> output.writeObject(map)));

        print("enumValue", serialize(output -> output.writeObject(Color.GREEN)));
        print("classValue", serialize(output -> output.writeObject(String.class)));
        print(
                "multiDimensionalArray",
                serialize(output -> output.writeObject(new String[][] {{"nested"}, null})));
        var proxy = Proxy.newProxyInstance(
                GenerateJavaCodecFixtures.class.getClassLoader(),
                new Class<?>[] {FixtureProxyInterface.class},
                new SerializableInvocationHandler());
        print("dynamicProxy", serialize(output -> output.writeObject(proxy)));

        var shared = new String("shared");
        print("sharedReference", serialize(output -> output.writeObject(new SharedPair(shared, shared))));

        var self = new SelfRecord();
        self.name = "root";
        self.self = self;
        print("selfReference", serialize(output -> output.writeObject(self)));

        print("multipleTopLevel", serialize(output -> {
            output.writeObject("first");
            output.writeObject(new int[] {1, 2});
        }));

        print("reset", serialize(output -> {
            var value = new String("again");
            output.writeObject(value);
            output.writeObject(value);
            output.reset();
            output.writeObject(value);
        }));

        print("customWriteObject", serialize(output -> output.writeObject(new CustomRecord())));
        print(
                "customWithoutDefaultFields",
                serialize(output -> output.writeObject(new NoDefaultRecord())));
        print(
                "customWithInterleavedDefaultFields",
                serialize(output -> output.writeObject(new InterleavedDefaultRecord())));
        print("externalizableProtocol2", serialize(output -> output.writeObject(new ExternalRecord())));
        print(
                "externalizableProtocol1",
                serialize(
                        ObjectStreamConstants.PROTOCOL_VERSION_1,
                        output -> output.writeObject(new ExternalRecord())));
        print("fatalException", fatalExceptionStream());
    }

    private static class BaseRecord implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        String label = "base";
        int number = 7;
    }

    private static final class ChildRecord extends BaseRecord {
        @Serial
        private static final long serialVersionUID = 1L;

        String label = "child";
        int number = 9;
    }

    private enum Color {
        RED,
        GREEN
    }

    private interface FixtureProxyInterface extends Serializable {
        String echo(String value);
    }

    private static final class SerializableInvocationHandler
            implements InvocationHandler, Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        String label = "proxy-handler";

        @Override
        public Object invoke(Object proxy, Method method, Object[] args) {
            return args == null || args.length == 0 ? null : args[0];
        }
    }

    private static final class SharedPair implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        Object left;
        Object right;

        SharedPair(Object left, Object right) {
            this.left = left;
            this.right = right;
        }
    }

    private static final class SelfRecord implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        String name;
        SelfRecord self;
    }

    private static final class NestedRecord implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        String value;

        NestedRecord(String value) {
            this.value = value;
        }
    }

    private static final class CustomRecord implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        int count = 3;
        String name = "custom";

        @Serial
        private void writeObject(ObjectOutputStream output) throws IOException {
            output.defaultWriteObject();
            output.writeInt(0x10203040);
            output.writeUTF("tail");
            output.writeObject(new NestedRecord("inside"));
        }

        @Serial
        private void readObject(ObjectInputStream input) throws IOException, ClassNotFoundException {
            input.defaultReadObject();
            input.readInt();
            input.readUTF();
            input.readObject();
        }
    }

    /** Declares ordinary fields but deliberately never writes them. */
    private static final class NoDefaultRecord implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        int count = 41;
        String name = "not-written";

        @Serial
        private void writeObject(ObjectOutputStream output) throws IOException {
            output.writeInt(0x11223344);
            output.writeObject(new NestedRecord("only-custom"));
        }

        @Serial
        private void readObject(ObjectInputStream input) throws IOException, ClassNotFoundException {
            input.readInt();
            input.readObject();
        }
    }

    /** Proves that defaultWriteObject may occur between two groups of optional data. */
    private static final class InterleavedDefaultRecord implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        int count = 5;
        String name = "middle";

        @Serial
        private void writeObject(ObjectOutputStream output) throws IOException {
            output.writeInt(0x01020304);
            output.writeObject(new NestedRecord("before"));
            output.defaultWriteObject();
            output.writeLong(0x1112131415161718L);
            output.writeObject(new NestedRecord("after"));
        }

        @Serial
        private void readObject(ObjectInputStream input) throws IOException, ClassNotFoundException {
            input.readInt();
            input.readObject();
            input.defaultReadObject();
            input.readLong();
            input.readObject();
        }
    }

    private static final class FailingRecord implements Serializable {
        @Serial
        private static final long serialVersionUID = 1L;

        int marker = 7;

        @Serial
        private void writeObject(ObjectOutputStream output) throws IOException {
            output.defaultWriteObject();
            output.writeInt(0x21222324);
            output.writeObject("before-failure");

            var failure = new IOException("fixture failure", null);
            failure.setStackTrace(new StackTraceElement[0]);
            throw failure;
        }
    }

    public static final class ExternalRecord implements Externalizable {
        @Serial
        private static final long serialVersionUID = 1L;

        public ExternalRecord() {}

        @Override
        public void writeExternal(ObjectOutput output) throws IOException {
            output.writeInt(0x50607080);
            output.writeUTF("external");
            output.writeObject(new NestedRecord("nested"));
        }

        @Override
        public void readExternal(ObjectInput input) throws IOException, ClassNotFoundException {
            input.readInt();
            input.readUTF();
            input.readObject();
        }
    }
}
