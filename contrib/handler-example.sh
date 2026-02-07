#!/bin/bash
# Example event handler for s1500d.
#
# Legacy mode — receives the event name as $1:
#   device-arrived, device-left,
#   paper-in, paper-out,
#   button-down, button-up
#
# Config mode — receives:
#   scan <profile>   (gesture completed)
#   device-arrived, device-left, paper-in, paper-out

EVENT="$1"
PROFILE="${2:-}"

case "$EVENT" in
    scan)
        logger -t s1500d "Scan gesture: profile=$PROFILE"
        # Your scan logic here — scanimage is safe to call,
        # s1500d has released the USB device.
        ;;
    paper-in)
        logger -t s1500d "Paper detected"
        ;;
    button-down)
        logger -t s1500d "Scan button pressed (legacy mode)"
        ;;
    device-arrived)
        logger -t s1500d "Scanner lid opened"
        ;;
    device-left)
        logger -t s1500d "Scanner lid closed"
        ;;
    *)
        logger -t s1500d "Event: $EVENT"
        ;;
esac
