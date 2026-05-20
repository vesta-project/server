package com.vesta.loader;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

public final class ModLog {

    static final Logger log = LoggerFactory.getLogger("VestaSync");

    static void info(String msg, Object... args) {
        log.info(msg, args);
    }

    static void warn(String msg, Object... args) {
        log.warn(msg, args);
    }

    static void error(String msg, Throwable t) {
        log.error(msg, t);
    }

    static void error(String msg) {
        log.error(msg);
    }

    private ModLog() {}
}
