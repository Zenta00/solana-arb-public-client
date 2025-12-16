#!/bin/bash

LOG_FILE="output.log"
CHECK_INTERVAL=180  # 3 minutes in seconds
CMD="nohup /home/thsmg/solana-arb-client/target/alternate/release/arbitrage > $LOG_FILE 2>&1 &"

# Start the program initially
eval $CMD
echo "Started arbitrage program at $(date)"

while true; do
    sleep $CHECK_INTERVAL
    
    # Check if the log file exists
    if [ ! -f "$LOG_FILE" ]; then
        echo "Log file not found. Restarting program..."
        eval $CMD
        continue
    fi
    
    # Extract the timestamp of the last transaction
    LAST_TIMESTAMP=$(grep -a "Timestamp:" "$LOG_FILE" | tail -1 | sed 's/Timestamp: //')
    
    if [ -z "$LAST_TIMESTAMP" ]; then
        echo "No transaction timestamps found in log. Waiting..."
        continue
    fi
    
    # Remove milliseconds from timestamp before parsing
    TIMESTAMP_NO_MS=$(echo "$LAST_TIMESTAMP" | sed 's/\.[0-9]*$//')
    
    # Convert timestamp to seconds since epoch
    LAST_TX_TIME=$(date -d "$TIMESTAMP_NO_MS" +%s)
    CURRENT_TIME=$(date +%s)

    
    # Calculate time difference
    TIME_DIFF=$((CURRENT_TIME - LAST_TX_TIME))
    
    echo "Last transaction was $TIME_DIFF seconds ago at $LAST_TIMESTAMP"
    
    # If last transaction was more than 3 minutes ago, restart
    if [ $TIME_DIFF -gt $CHECK_INTERVAL ]; then
        echo "No recent transactions detected. Restarting program..."
        
        # Kill the existing process
        pkill -f "/home/thsmg/solana-arb-client/target/alternate/release/arbitrage"
        
        # Wait a moment for the process to terminate
        sleep 2
        
        # Restart the program
        eval $CMD
        echo "Restarted arbitrage program at $(date)"
    fi
done
