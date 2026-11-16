// pam-try [-s SERVICE] [-u USER] [ANSWER...]
// Runs pam_authenticate and prints every conversation message. Each prompt
// takes the next ANSWER. A prompt with no answer left fails with PAM_CONV_ERR.
// Exits 0 on PAM_SUCCESS and 1 otherwise.

#include <pwd.h>
#include <security/pam_appl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

struct answers {
    char **next;
    char **end;
};

static int converse(int n, const struct pam_message **msg, struct pam_response **resp, void *data) {
    struct answers *a = data;
    struct pam_response *r = calloc(n, sizeof *r);

    if (!r)
        return PAM_BUF_ERR;
    for (int i = 0; i < n; i++) {
        switch (msg[i]->msg_style) {
        case PAM_PROMPT_ECHO_OFF:
        case PAM_PROMPT_ECHO_ON:
            printf("prompt: %s\n", msg[i]->msg);
            if (a->next == a->end) {
                printf("no answer left\n");
                fflush(stdout);
                for (int j = 0; j < i; j++)
                    free(r[j].resp);
                free(r);
                return PAM_CONV_ERR;
            }
            r[i].resp = strdup(*a->next++);
            break;
        case PAM_ERROR_MSG:
            printf("error: %s\n", msg[i]->msg);
            break;
        case PAM_TEXT_INFO:
            printf("info: %s\n", msg[i]->msg);
            break;
        }
        fflush(stdout);
    }
    *resp = r;
    return PAM_SUCCESS;
}

int main(int argc, char **argv) {
    const char *service = "deskshell-test";
    const char *user = NULL;
    struct answers a;
    struct pam_conv conv = {converse, &a};
    pam_handle_t *pamh = NULL;
    int opt, ret;

    while ((opt = getopt(argc, argv, "s:u:")) != -1) {
        switch (opt) {
        case 's':
            service = optarg;
            break;
        case 'u':
            user = optarg;
            break;
        default:
            fprintf(stderr, "usage: %s [-s SERVICE] [-u USER] [ANSWER...]\n", argv[0]);
            return 2;
        }
    }
    if (!user) {
        struct passwd *pw = getpwuid(getuid());

        if (!pw) {
            fprintf(stderr, "unknown uid\n");
            return 2;
        }
        user = pw->pw_name;
    }
    a.next = argv + optind;
    a.end = argv + argc;

    ret = pam_start(service, user, &conv, &pamh);
    if (ret == PAM_SUCCESS)
        ret = pam_authenticate(pamh, 0);
    printf("result: %s (%d)\n", pam_strerror(pamh, ret), ret);
    pam_end(pamh, ret);
    return ret == PAM_SUCCESS ? 0 : 1;
}
