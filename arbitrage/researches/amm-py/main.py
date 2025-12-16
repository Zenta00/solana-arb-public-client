from typing import List, Tuple, Union
import math
import random
import matplotlib.pyplot as plt
import numpy as np

# Constants
SCALE_FEE_CONSTANT = 100_000_000
BIN_SIZE = 10  # Number of ticks/bins for DLMM and CLMM
Q64 = 2**64

class RaydiumAMM:
    def __init__(self, reserve_x: int, reserve_y: int, fee: int):
        self.reserve_x = reserve_x
        self.reserve_y = reserve_y
        self.k = reserve_x * reserve_y
        self.fee = fee

    def get_amount_out(self, amount_in: int, x_for_y: bool) -> int:
        amount_in_with_fee = amount_in * (SCALE_FEE_CONSTANT - self.fee) // SCALE_FEE_CONSTANT
        
        if x_for_y:
            return self.reserve_y - (self.k // (self.reserve_x + amount_in_with_fee))
        else:
            return self.reserve_x - (self.k // (self.reserve_y + amount_in_with_fee))

class Bin:
    def __init__(self, price: int, amount_x: int, amount_y: int):
        self.price = price
        self.amount_x = amount_x
        self.amount_y = amount_y


fee_compounding_factor = 125
class DLMM:
    def __init__(self, bins: List[Bin], active_id: int, fee: int):
        self.bins = bins
        self.active_id = active_id
        self.fee = fee  # Base fee

    def get_amount_out(self, amount_in: int, x_for_y: bool) -> int:
        amount_out = 0
        current_bin_index = self.active_id
        amount_in_remaining = amount_in
        bins_crossed = 0  # Track number of bins crossed for compound fee

        if x_for_y:
            while amount_in_remaining > 0 and current_bin_index >= 0:
                # Apply compounding fee based on bins crossed
                current_fee = self.fee * (fee_compounding_factor ** bins_crossed) // 100
                amount_in_with_fee = amount_in_remaining * (SCALE_FEE_CONSTANT - current_fee) // SCALE_FEE_CONSTANT
                
                current_bin = self.bins[current_bin_index]
                bin_price = current_bin.price
                bin_reserve_y = current_bin.amount_y

                potential_amount_out = (amount_in_with_fee * bin_price) // Q64

                if potential_amount_out <= bin_reserve_y:
                    amount_out += potential_amount_out
                    amount_in_remaining = 0
                else:
                    amount_out += bin_reserve_y
                    amount_in_used = (bin_reserve_y * Q64) // bin_price
                    amount_in_remaining -= amount_in_used
                    bins_crossed += 1

                current_bin_index -= 1
        else:
            while amount_in_remaining > 0 and current_bin_index < BIN_SIZE:
                # Apply compounding fee based on bins crossed
                current_fee = self.fee * (fee_compounding_factor ** bins_crossed) // 100
                amount_in_with_fee = amount_in_remaining * (SCALE_FEE_CONSTANT - current_fee) // SCALE_FEE_CONSTANT
                
                current_bin = self.bins[current_bin_index]
                bin_price = current_bin.price
                bin_reserve_x = current_bin.amount_x

                potential_amount_out = (amount_in_with_fee * Q64) // bin_price

                if potential_amount_out <= bin_reserve_x:
                    amount_out += potential_amount_out
                    amount_in_remaining = 0
                else:
                    amount_out += bin_reserve_x
                    amount_in_used = (bin_reserve_x * bin_price) // Q64
                    amount_in_remaining -= amount_in_used
                    bins_crossed += 1

                current_bin_index += 1

        return amount_out

class Tick:
    def __init__(self, price: int, reserve_x: int, reserve_y: int, delta_liquidity: int):
        self.price = price
        self.reserve_x = reserve_x
        self.reserve_y = reserve_y
        self.delta_liquidity = delta_liquidity
        self.k = reserve_x * reserve_y

class CLMM:
    def __init__(self, ticks: List[Tick], active_tick_id: int, fee: int):
        self.ticks = ticks
        self.active_tick_id = active_tick_id
        self.fee = fee

    def get_amount_out(self, amount_in: int, x_for_y: bool) -> int:
        amount_out = 0
        amount_in_remaining = amount_in
        current_tick_index = self.active_tick_id

        while amount_in_remaining > 0 and 0 <= current_tick_index < len(self.ticks):
            current_tick = self.ticks[current_tick_index]
            
            # Apply fee
            amount_in_with_fee = amount_in_remaining * (SCALE_FEE_CONSTANT - self.fee) // SCALE_FEE_CONSTANT
            
            # Calculate amount out using constant product formula
            if x_for_y:
                tick_amount_out = current_tick.reserve_y - (current_tick.k // (current_tick.reserve_x + amount_in_with_fee))
                next_price = self.ticks[current_tick_index + 1].price if current_tick_index + 1 < len(self.ticks) else float('inf')
                
                # Check if we cross the next tick
                if (current_tick.reserve_x + amount_in_with_fee) / (current_tick.reserve_y - tick_amount_out) >= next_price:
                    # Calculate partial amount for this tick
                    max_x_in = (current_tick.reserve_x * next_price - current_tick.reserve_y) // (1 - next_price)
                    tick_amount_out = current_tick.reserve_y - (current_tick.k // (current_tick.reserve_x + max_x_in))
                    amount_in_remaining -= max_x_in
                    current_tick_index += 1
                    # Update liquidity when crossing tick
                    if current_tick_index < len(self.ticks):
                        self.ticks[current_tick_index].reserve_x += current_tick.delta_liquidity
                        self.ticks[current_tick_index].reserve_y += current_tick.delta_liquidity
                else:
                    amount_in_remaining = 0
            else:
                tick_amount_out = current_tick.reserve_x - (current_tick.k // (current_tick.reserve_y + amount_in_with_fee))
                prev_price = self.ticks[current_tick_index - 1].price if current_tick_index > 0 else 0
                
                # Check if we cross the previous tick
                if (current_tick.reserve_y + amount_in_with_fee) / (current_tick.reserve_x - tick_amount_out) <= prev_price:
                    # Calculate partial amount for this tick
                    max_y_in = (current_tick.reserve_y * prev_price - current_tick.reserve_x) // (1 - prev_price)
                    tick_amount_out = current_tick.reserve_x - (current_tick.k // (current_tick.reserve_y + max_y_in))
                    amount_in_remaining -= max_y_in
                    current_tick_index -= 1
                    # Update liquidity when crossing tick
                    if current_tick_index >= 0:
                        self.ticks[current_tick_index].reserve_x -= current_tick.delta_liquidity
                        self.ticks[current_tick_index].reserve_y -= current_tick.delta_liquidity
                else:
                    amount_in_remaining = 0
            
            amount_out += tick_amount_out

        return amount_out

def initialize_v4_with_random_liquidity(price: int) -> RaydiumAMM:
    # Find reserves where reserve_y / reserve_x = price/Q64
    # Using min reserve of 1_000_000_000
    reserve_x = random.randint(1_000_000_000, 10_000_000_000)
    reserve_y = (reserve_x * price) // Q64
    
    # Ensure reserve_y meets minimum
    if reserve_y < 1_000_000_000:
        reserve_y = 1_000_000_000
        reserve_x = (reserve_y * Q64) // price
    
    return RaydiumAMM(reserve_x, reserve_y, fee=30)  # 0.3% fee

def initialize_dlmm_with_random_liquidity(price: int, active_id: int = 5) -> DLMM:
    bins = []
    price_factor = 1.05
    active_price = price
    
    # Calculate prices for all bins
    for i in range(BIN_SIZE):
        bin_price = int(active_price * (price_factor ** (i - active_id)))
        
        if i == active_id:
            # Active bin has both reserves non-zero
            amount_x = random.randint(1_000_000_000, 10_000_000_000)
            amount_y = random.randint(1_000_000_000, 10_000_000_000)
        elif i < active_id:
            # Bins below active have y=0
            amount_x = random.randint(1_000_000_000, 10_000_000_000)
            amount_y = 0
        else:
            # Bins above active have x=0
            amount_x = 0
            amount_y = random.randint(1_000_000_000, 10_000_000_000)
            
        bins.append(Bin(bin_price, amount_x, amount_y))
    
    return DLMM(bins, active_id, fee=30)

def initialize_clmm_with_random_liquidity(price: int, active_tick_id: int = 5) -> CLMM:
    ticks = []
    price_factor = 1.05
    active_price = price
    
    # Create ticks with random delta liquidity
    for i in range(BIN_SIZE):
        tick_price = int(active_price * (price_factor ** (i - active_tick_id)))
        
        # Random base reserves
        reserve_x = random.randint(1_000_000_000, 10_000_000_000)
        reserve_y = random.randint(1_000_000_000, 10_000_000_000)
        
        # Random delta liquidity (positive or negative)
        delta_liquidity = random.randint(1_000_000_000, 5_000_000_000)
        if random.random() < 0.5:
            delta_liquidity = -delta_liquidity
            
        ticks.append(Tick(tick_price, reserve_x, reserve_y, delta_liquidity))
    
    return CLMM(ticks, active_tick_id, fee=30)

def profit(amm_path: List[Tuple[Union[RaydiumAMM, DLMM, CLMM], bool]], initial_amount: int) -> int:
    """
    Calculate profit from a series of trades across different AMMs.
    
    Args:
        amm_path: List of tuples containing (AMM instance, x_for_y flag)
        initial_amount: Initial amount to trade with
    
    Returns:
        Final amount after all trades
    """
    current_amount = initial_amount
    
    for amm, x_for_y in amm_path:
        current_amount = amm.get_amount_out(current_amount, x_for_y)
    
    return current_amount

def plot_profit_curve():
    # Create sample AMMs with random liquidity
    initial_price = Q64  # Price of 1.0
    v4_amm = initialize_v4_with_random_liquidity(initial_price)
    dlmm_amm = initialize_dlmm_with_random_liquidity(initial_price)
    clmm_amm = initialize_clmm_with_random_liquidity(initial_price)

    # Define a simple arbitrage path (example: V4 -> DLMM -> V4)
    path = [
        (v4_amm, True),      # X for Y
        (dlmm_amm, False),   # Y for X
        (v4_amm, True)       # X for Y
    ]

    # Create input amounts array
    amounts = np.arange(0, 10_000_000_000 + 1, 1_000)
    profits = [profit(path, amount) - amount for amount in amounts]  # Subtract initial amount to get actual profit

    # Create the plot
    plt.figure(figsize=(12, 6))
    plt.plot(amounts, profits, label='Profit')
    plt.xlabel('Initial Amount')
    plt.ylabel('Profit Amount')
    plt.title('Arbitrage Profit vs Initial Amount')
    plt.grid(True)
    plt.legend()
    plt.show()

# Example usage:
initial_price_v4 = Q64  # Example price of 0.1
initial_price_dlmm = Q64*1.1  # Example price of 0.1
initial_price_clmm = Q64*1.2  # Example price of 0.1
v4_amm = initialize_v4_with_random_liquidity(initial_price_v4)
dlmm_amm = initialize_dlmm_with_random_liquidity(initial_price_dlmm)


plot_profit_curve()




