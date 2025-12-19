package presets

import (
	"github.com/ethereum-optimism/optimism/op-devstack/devtest"
	"github.com/ethereum-optimism/optimism/op-devstack/presets"
	"github.com/ethereum-optimism/optimism/op-devstack/stack"
	"github.com/ethereum-optimism/optimism/op-devstack/sysgo"
	"github.com/ethereum-optimism/optimism/op-service/eth"
)

// FaultProofConfig holds configuration for fault proof proposer tests.
type FaultProofConfig struct {
	ProposalIntervalInBlocks uint64
	FetchInterval            uint64
	RangeSplitCount          uint64
	MaxConcurrentRangeProofs uint64
	FastFinalityMode         bool
	FastFinalityProvingLimit uint64
}

// DefaultFaultProofConfig returns the default configuration.
func DefaultFaultProofConfig() FaultProofConfig {
	return FaultProofConfig{
		ProposalIntervalInBlocks: 10,
		FetchInterval:            1,
		RangeSplitCount:          1,
		MaxConcurrentRangeProofs: 1,
		FastFinalityMode:         false,
		FastFinalityProvingLimit: 1,
	}
}

// FastFinalityFaultProofConfig returns configuration with fast finality mode enabled.
func FastFinalityFaultProofConfig() FaultProofConfig {
	cfg := DefaultFaultProofConfig()
	cfg.FastFinalityMode = true
	return cfg
}

// WithZisKFPProposer creates a fault proof proposer with custom configuration.
func WithZisKFPProposer(dest *sysgo.DefaultSingleChainInteropSystemIDs, cfg FaultProofConfig) stack.CommonOption {
	return withZisKPreset(dest, func(opt *stack.CombinedOption[*sysgo.Orchestrator], ids sysgo.DefaultSingleChainInteropSystemIDs, l2ChainID eth.ChainID) {
		opt.Add(sysgo.WithSuperDeploySP1MockVerifier(ids.L1EL, l2ChainID))
		opt.Add(sysgo.WithSuperDeployOPZisKFaultDisputeGame(ids.L1CL, ids.L1EL, ids.L2ACL, ids.L2AEL, sysgo.WithFdgL2StartingBlockNumber(1)))
		opt.Add(sysgo.WithSuperZisKFaultProofProposer(ids.L2AProposer, ids.L1CL, ids.L1EL, ids.L2ACL, ids.L2AEL,
			sysgo.WithFPProposalIntervalInBlocks(cfg.ProposalIntervalInBlocks),
			sysgo.WithFPFetchInterval(cfg.FetchInterval),
			sysgo.WithFPRangeSplitCount(cfg.RangeSplitCount),
			sysgo.WithFPMaxConcurrentRangeProofs(cfg.MaxConcurrentRangeProofs),
			sysgo.WithFPFastFinalityMode(cfg.FastFinalityMode),
			sysgo.WithFPFastFinalityProvingLimit(cfg.FastFinalityProvingLimit)))
	})
}

// WithDefaultZisKFPProposer creates a fault proof proposer with default configuration.
func WithDefaultZisKFPProposer(dest *sysgo.DefaultSingleChainInteropSystemIDs) stack.CommonOption {
	return WithZisKFPProposer(dest, DefaultFaultProofConfig())
}

// WithZisKFPProposerFastFinality creates a fault proof proposer optimized for fast finality.
func WithZisKFPProposerFastFinality(dest *sysgo.DefaultSingleChainInteropSystemIDs) stack.CommonOption {
	return WithZisKFPProposer(dest, FastFinalityFaultProofConfig())
}

// NewFaultProofSystem creates a new fault proof test system with custom configuration.
// This allows per-test configuration instead of relying on TestMain.
func NewFaultProofSystem(t devtest.T, cfg FaultProofConfig) *presets.MinimalWithProposer {
	var ids sysgo.DefaultSingleChainInteropSystemIDs
	return NewSystem(t, WithZisKFPProposer(&ids, cfg))
}
