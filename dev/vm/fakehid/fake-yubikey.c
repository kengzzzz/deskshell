// Creates a uhid device that looks like a YubiKey (USB 1050:0407, FIDO usage
// page) and keeps it until SIGTERM or SIGINT. The kernel removes a uhid device
// when its fd closes, so the device lives exactly as long as this process.

#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <linux/uhid.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static const unsigned char rdesc[] = {
    0x06, 0xd0, 0xf1, // usage page (FIDO alliance)
    0x09, 0x01,       // usage (U2F authenticator device)
    0xa1, 0x01,       // collection (application)
    0x09, 0x20,       //   usage (input report data)
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40,
    0x81, 0x02,       //   input (data, var, abs), 64 bytes
    0x09, 0x21,       //   usage (output report data)
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x40,
    0x91, 0x02,       //   output (data, var, abs), 64 bytes
    0xc0,             // end collection
};

static volatile sig_atomic_t stop;

static void on_signal(int sig) {
    (void)sig;
    stop = 1;
}

static int send_event(int fd, const struct uhid_event *ev) {
    return write(fd, ev, sizeof *ev) == (ssize_t)sizeof *ev ? 0 : -1;
}

int main(void) {
    struct sigaction sa = {.sa_handler = on_signal};
    struct uhid_event ev;
    int fd;

    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);

    fd = open("/dev/uhid", O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        perror("open /dev/uhid");
        return 1;
    }

    memset(&ev, 0, sizeof ev);
    ev.type = UHID_CREATE2;
    snprintf((char *)ev.u.create2.name, sizeof ev.u.create2.name, "Yubico YubiKey OTP+FIDO+CCID");
    snprintf((char *)ev.u.create2.phys, sizeof ev.u.create2.phys, "fake-yubikey/input0");
    memcpy(ev.u.create2.rd_data, rdesc, sizeof rdesc);
    ev.u.create2.rd_size = sizeof rdesc;
    ev.u.create2.bus = BUS_USB;
    ev.u.create2.vendor = 0x1050;
    ev.u.create2.product = 0x0407;
    ev.u.create2.version = 0x0100;
    if (send_event(fd, &ev) != 0) {
        perror("UHID_CREATE2");
        return 1;
    }

    while (!stop) {
        struct uhid_event in;
        ssize_t n = read(fd, &in, sizeof in);

        if (n < 0) {
            if (errno == EINTR)
                continue;
            perror("read /dev/uhid");
            break;
        }
        memset(&ev, 0, sizeof ev);
        if (in.type == UHID_GET_REPORT) {
            ev.type = UHID_GET_REPORT_REPLY;
            ev.u.get_report_reply.id = in.u.get_report.id;
            ev.u.get_report_reply.err = EIO;
            send_event(fd, &ev);
        } else if (in.type == UHID_SET_REPORT) {
            ev.type = UHID_SET_REPORT_REPLY;
            ev.u.set_report_reply.id = in.u.set_report.id;
            ev.u.set_report_reply.err = EIO;
            send_event(fd, &ev);
        }
    }

    memset(&ev, 0, sizeof ev);
    ev.type = UHID_DESTROY;
    send_event(fd, &ev);
    close(fd);
    return 0;
}
