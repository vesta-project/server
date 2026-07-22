package com.vesta.wrapper.security;

import com.vesta.wrapper.config.WrapperConfig;
import org.bouncycastle.asn1.x500.X500Name;
import org.bouncycastle.asn1.x509.BasicConstraints;
import org.bouncycastle.asn1.x509.Extension;
import org.bouncycastle.asn1.x509.ExtendedKeyUsage;
import org.bouncycastle.asn1.x509.GeneralName;
import org.bouncycastle.asn1.x509.GeneralNames;
import org.bouncycastle.asn1.x509.KeyPurposeId;
import org.bouncycastle.asn1.x509.KeyUsage;
import org.bouncycastle.cert.X509v3CertificateBuilder;
import org.bouncycastle.cert.jcajce.JcaX509CertificateConverter;
import org.bouncycastle.cert.jcajce.JcaX509v3CertificateBuilder;
import org.bouncycastle.operator.ContentSigner;
import org.bouncycastle.operator.jcajce.JcaContentSignerBuilder;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.io.InputStream;
import java.io.OutputStream;
import java.math.BigInteger;
import java.net.InetAddress;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.security.cert.Certificate;
import java.security.cert.X509Certificate;
import java.time.Instant;
import java.time.temporal.ChronoUnit;
import java.util.Date;
import java.util.Set;

/** Creates the persistent TLS identity used by fresh self-hosted wrappers. */
public final class TlsMaterial {
    private static final Logger log = LoggerFactory.getLogger(TlsMaterial.class);

    /**
     * Computes a stable SHA-256 fingerprint for the wrapper's self-hosted TLS identity.
     *
     * <p>The fingerprint is based on the DER-encoded certificate bytes of the persistent
     * {@code "vesta"} entry in the PKCS#12 keystore.
     */
    public static String sha256Fingerprint(WrapperConfig config) throws Exception {
        Path target = config.tlsKeyStore.toAbsolutePath().normalize();
        if (!Files.exists(target)) {
            ensure(config);
        }

        KeyStore keyStore = KeyStore.getInstance("PKCS12");
        try (InputStream input = Files.newInputStream(target)) {
            keyStore.load(input, config.tlsKeyStorePassword.toCharArray());
        }

        Certificate cert = keyStore.getCertificate("vesta");
        if (cert == null) {
            throw new IllegalStateException("TLS keystore has no 'vesta' certificate");
        }

        byte[] encoded = cert.getEncoded();
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        byte[] hash = digest.digest(encoded);
        return toHex(hash);
    }

    private static String toHex(byte[] bytes) {
        StringBuilder sb = new StringBuilder(bytes.length * 2);
        for (byte b : bytes) {
            sb.append(String.format(java.util.Locale.ROOT, "%02x", b));
        }
        return sb.toString();
    }

    public static Path ensure(WrapperConfig config) throws Exception {
        Path target = config.tlsKeyStore.toAbsolutePath().normalize();
        if (Files.exists(target)) {
            validate(target, config.tlsKeyStorePassword);
            return target;
        }
        if (!config.tlsAutoGenerate) {
            throw new IllegalStateException("TLS keystore does not exist: " + target);
        }
        Files.createDirectories(target.getParent());
        generate(target, config);
        validate(target, config.tlsKeyStorePassword);
        log.info("Generated persistent self-hosted TLS identity at {}", target);
        return target;
    }

    private static void generate(Path target, WrapperConfig config) throws Exception {
        KeyPairGenerator keys = KeyPairGenerator.getInstance("RSA");
        keys.initialize(2048, new SecureRandom());
        KeyPair pair = keys.generateKeyPair();
        Instant now = Instant.now();
        X500Name subject = new X500Name("CN=Vesta Server " + config.serverId);
        X509v3CertificateBuilder certificateBuilder = new JcaX509v3CertificateBuilder(
                subject,
                new BigInteger(160, new SecureRandom()).abs(),
                Date.from(now.minus(5, ChronoUnit.MINUTES)),
                Date.from(now.plus(3650, ChronoUnit.DAYS)),
                subject,
                pair.getPublic()
        );
        certificateBuilder.addExtension(Extension.basicConstraints, true, new BasicConstraints(false));
        certificateBuilder.addExtension(
                Extension.keyUsage,
                true,
                new KeyUsage(KeyUsage.digitalSignature | KeyUsage.keyEncipherment)
        );
        certificateBuilder.addExtension(
                Extension.extendedKeyUsage,
                false,
                new ExtendedKeyUsage(KeyPurposeId.id_kp_serverAuth)
        );
        certificateBuilder.addExtension(
                Extension.subjectAlternativeName,
                false,
                new GeneralNames(subjectAlternativeName(config.publicHost))
        );
        ContentSigner signer = new JcaContentSignerBuilder("SHA256withRSA")
                .build(pair.getPrivate());
        X509Certificate certificate = new JcaX509CertificateConverter()
                .getCertificate(certificateBuilder.build(signer));
        certificate.checkValidity();
        certificate.verify(pair.getPublic());

        char[] password = config.tlsKeyStorePassword.toCharArray();
        KeyStore keyStore = KeyStore.getInstance("PKCS12");
        keyStore.load(null, password);
        keyStore.setKeyEntry(
                "vesta",
                pair.getPrivate(),
                password,
                new Certificate[]{certificate}
        );

        Path temporary = target.resolveSibling(target.getFileName() + ".tmp");
        try (OutputStream output = Files.newOutputStream(temporary)) {
            keyStore.store(output, password);
        }
        try {
            Files.move(
                    temporary,
                    target,
                    StandardCopyOption.ATOMIC_MOVE,
                    StandardCopyOption.REPLACE_EXISTING
            );
        } catch (AtomicMoveNotSupportedException ignored) {
            Files.move(temporary, target, StandardCopyOption.REPLACE_EXISTING);
        }
        try {
            Files.setPosixFilePermissions(target, Set.of(
                    java.nio.file.attribute.PosixFilePermission.OWNER_READ,
                    java.nio.file.attribute.PosixFilePermission.OWNER_WRITE
            ));
        } catch (UnsupportedOperationException ignored) {
            // Windows protects the file through the containing user's ACL.
        }
    }

    private static GeneralName subjectAlternativeName(String host) {
        String value = host == null || host.isBlank() ? "localhost" : host.trim();
        try {
            InetAddress.getByName(value);
            if (value.matches("[0-9a-fA-F:.]+")) {
                return new GeneralName(GeneralName.iPAddress, value);
            }
        } catch (Exception ignored) {
        }
        return new GeneralName(GeneralName.dNSName, value);
    }

    private static void validate(Path path, String password) throws Exception {
        KeyStore keyStore = KeyStore.getInstance("PKCS12");
        try (var input = Files.newInputStream(path)) {
            keyStore.load(input, password.toCharArray());
        }
        if (!keyStore.isKeyEntry("vesta")) {
            throw new IllegalStateException("TLS keystore has no 'vesta' private key");
        }
    }

    private TlsMaterial() {
    }
}
