#!/bin/bash

LOG_FILE="output.log"
CHECK_INTERVAL=90  # Time in seconds
CMD="nohup /home/thsmg/solana-arb-client/target/release/arbitrage > $LOG_FILE 2>&1 &"

# Start the program initially
eval $CMD
LAST_MOD_TIME=$(stat -c %Y "$LOG_FILE")  # Get last modified time

while true; do
    sleep $CHECK_INTERVAL

    # Get the last modified time of the log file
    NEW_MOD_TIME=$(stat -c %Y "$LOG_FILE")

    if [[ "$LAST_MOD_TIME" == "$NEW_MOD_TIME" ]]; then
        echo "No updates in log file. Restarting program..."
        
        # Kill the existing process
        pkill -f "/home/thsmg/solana-arb-client/target/release/arbitrage"

        # Restart the program
        eval $CMD

        # Update last modification time
        LAST_MOD_TIME=$(stat -c %Y "$LOG_FILE")
    else
        LAST_MOD_TIME=$NEW_MOD_TIME
    fi
done
