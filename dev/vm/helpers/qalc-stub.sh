#!/bin/sh
# A slow fake qalc for the test guest: logs its argv, waits 2 s, then answers.
echo "qalc $*" >> /tmp/qalc.log
sleep 2
echo "$(($3)) from qalc"
