//! The contract functions this adapter calls. `ICswapRouter` is our own fee-taking router;
//! its shape must match the deployed contract.

alloy_sol_types::sol! {
    struct PoolKey {
        address currency0;
        address currency1;
        uint24 fee;
        int24 tickSpacing;
        address hooks;
    }

    interface IUniswapV2Factory {
        function getPair(address tokenA, address tokenB) external view returns (address pair);
    }

    interface IUniswapV2Pair {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }

    interface IUniswapV3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    }

    interface IUniswapV3Pool {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function liquidity() external view returns (uint128);
        function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked);
    }

    interface IQuoterV2 {
        struct QuoteExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint24 fee;
            uint160 sqrtPriceLimitX96;
        }
        function quoteExactInputSingle(QuoteExactInputSingleParams memory params) external returns (uint256 amountOut, uint160 sqrtPriceX96After, uint32 initializedTicksCrossed, uint256 gasEstimate);
    }

    interface IPositionManager {
        /// Keys are stored under the first 25 bytes of the pool id.
        function poolKeys(bytes25 poolId) external view returns (PoolKey memory poolKey);
    }

    interface IStateView {
        function getSlot0(bytes32 poolId) external view returns (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 lpFee);
        function getLiquidity(bytes32 poolId) external view returns (uint128 liquidity);
    }

    interface IV4Quoter {
        struct QuoteExactSingleParams {
            PoolKey poolKey;
            bool zeroForOne;
            uint128 exactAmount;
            bytes hookData;
        }
        function quoteExactInputSingle(QuoteExactSingleParams memory params) external returns (uint256 amountOut, uint256 gasEstimate);
    }

    interface IERC20 {
        function allowance(address owner, address spender) external view returns (uint256);
        function approve(address spender, uint256 amount) external returns (bool);
    }

    interface ICswapRouter {
        enum Version { V2, V3, V4 }
        struct Pool {
            Version version;
            address pool;
            PoolKey v4Key;
        }
        function buy(Pool calldata pool, address token, uint256 minOut, uint16 feeBps, uint256 deadline) external payable returns (uint256 tokensOut);
        function sell(Pool calldata pool, address token, uint256 amountIn, uint256 minOut, uint16 feeBps, uint256 deadline) external returns (uint256 ethOut);
    }
}
