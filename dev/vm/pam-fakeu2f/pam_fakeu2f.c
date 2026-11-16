// Test stand-in for pam_u2f with a PIN. Never install it outside a test VM.
//
// The key counts as present when a hidraw device with USB vendor 1050 exists.
// Without a key it fails at once and prompts for nothing, like pam_u2f.
// With a key it asks for the PIN, sends the touch cue (with the `cue` option),
// waits `touch_delay_ms` outside any conversation callback, and succeeds when
// the PIN matches the `pin` line of the config file.
//
// Options: cue, conf=PATH (default /etc/fakeu2f.conf).
// The prompt and cue strings are the ones pam_u2f uses.

#define PAM_SM_AUTH
#include <glob.h>
#include <security/pam_ext.h>
#include <security/pam_modules.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <syslog.h>
#include <time.h>

#define PIN_PROMPT "Please enter the PIN: "
#define CUE_PROMPT "Please touch the FIDO authenticator."
#define DEFAULT_CONF "/etc/fakeu2f.conf"

static int key_present(void) {
    glob_t g;
    int found = 0;

    if (glob("/sys/class/hidraw/*/device/uevent", 0, NULL, &g) != 0)
        return 0;
    for (size_t i = 0; i < g.gl_pathc && !found; i++) {
        FILE *f = fopen(g.gl_pathv[i], "r");
        char line[256];

        if (!f)
            continue;
        while (fgets(line, sizeof line, f)) {
            unsigned bus, vendor, product;

            if (sscanf(line, "HID_ID=%x:%x:%x", &bus, &vendor, &product) == 3 && vendor == 0x1050) {
                found = 1;
                break;
            }
        }
        fclose(f);
    }
    globfree(&g);
    return found;
}

static int read_conf(const char *path, char *pin, size_t pin_len, long *touch_ms) {
    FILE *f = fopen(path, "r");
    char line[256];

    if (!f)
        return -1;
    pin[0] = '\0';
    *touch_ms = 0;
    while (fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = '\0';
        if (strncmp(line, "pin=", 4) == 0)
            snprintf(pin, pin_len, "%s", line + 4);
        else if (strncmp(line, "touch_delay_ms=", 15) == 0)
            *touch_ms = strtol(line + 15, NULL, 10);
    }
    fclose(f);
    return pin[0] ? 0 : -1;
}

static void sleep_ms(long ms) {
    struct timespec ts = {.tv_sec = ms / 1000, .tv_nsec = (ms % 1000) * 1000000L};

    while (nanosleep(&ts, &ts) != 0)
        ;
}

PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    const char *conf = DEFAULT_CONF;
    int cue = 0;
    char pin[256];
    long touch_ms;
    char *resp = NULL;
    int match;

    (void)flags;
    for (int i = 0; i < argc; i++) {
        if (strcmp(argv[i], "cue") == 0)
            cue = 1;
        else if (strncmp(argv[i], "conf=", 5) == 0)
            conf = argv[i] + 5;
    }

    if (!key_present()) {
        pam_syslog(pamh, LOG_INFO, "no key present");
        return PAM_AUTH_ERR;
    }
    if (read_conf(conf, pin, sizeof pin, &touch_ms) != 0) {
        pam_syslog(pamh, LOG_ERR, "cannot read pin from %s", conf);
        return PAM_AUTH_ERR;
    }

    if (pam_prompt(pamh, PAM_PROMPT_ECHO_OFF, &resp, "%s", PIN_PROMPT) != PAM_SUCCESS || !resp) {
        free(resp);
        return PAM_AUTH_ERR;
    }
    if (cue)
        pam_info(pamh, "%s", CUE_PROMPT);

    match = strcmp(resp, pin) == 0;
    explicit_bzero(resp, strlen(resp));
    free(resp);
    if (!match) {
        pam_syslog(pamh, LOG_INFO, "wrong pin");
        return PAM_AUTH_ERR;
    }

    if (touch_ms > 0)
        sleep_ms(touch_ms);
    if (!key_present()) {
        pam_syslog(pamh, LOG_INFO, "key removed before touch");
        return PAM_AUTH_ERR;
    }
    return PAM_SUCCESS;
}

PAM_EXTERN int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh;
    (void)flags;
    (void)argc;
    (void)argv;
    return PAM_SUCCESS;
}
