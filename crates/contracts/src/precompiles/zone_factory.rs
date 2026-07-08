//! `ZoneFactory` — deployed on Tempo L1.

pub use ZoneFactory::ZoneInfo;

crate::sol! {
    #[derive(Debug)]
    contract ZoneFactory {
        struct ZoneInfo {
            uint32 zoneId;
            address portal;
            address messenger;
            address initialToken;
            address admin;
            address sequencer;
            address verifier;
            bytes32 genesisBlockHash;
            bytes32 genesisTempoBlockHash;
            uint64 genesisTempoBlockNumber;
            string rpcUrl;
        }
        struct ZoneParams {
            bytes32 genesisBlockHash;
            bytes32 genesisTempoBlockHash;
            uint64 genesisTempoBlockNumber;
        }
        struct CreateZoneParams {
            address initialToken;
            address admin;
            address sequencer;
            address verifier;
            ZoneParams zoneParams;
            string rpcUrl;
        }
        event ZoneCreated(
            uint32 indexed zoneId,
            address indexed portal,
            address indexed messenger,
            address initialToken,
            address admin,
            address sequencer,
            address verifier,
            bytes32 genesisBlockHash,
            bytes32 genesisTempoBlockHash,
            uint64 genesisTempoBlockNumber
        );
        event VerifierRegistered(address indexed verifier);
        event VerifierUnregistered(address indexed verifier);
        event VerifierUpdated(address indexed previousVerifier, address indexed verifier);
        event ForkVerifierUpdated(
            address indexed forkVerifier,
            uint64 forkActivationBlock,
            uint64 protocolVersion
        );
        function createZone(CreateZoneParams calldata params) external returns (uint32 zoneId, address portal);
        function verifier() external view returns (address);
        function forkVerifier() external view returns (address);
        function forkActivationBlock() external view returns (uint64);
        function protocolVersion() external view returns (uint64);
        function zones(uint32 zoneId) external view returns (ZoneInfo memory);
        function zoneCount() external view returns (uint32);
        function zonePortalCount() external view returns (uint256);
        function zonePortalAt(uint256 index) external view returns (address);
        function isZonePortal(address portal) external view returns (bool);
        function isZoneMessenger(address messenger) external view returns (bool);
        function isValidVerifier(address verifier) external view returns (bool);
        function registerVerifier(address verifier) external;
        function unregisterVerifier(address verifier) external;
        function setVerifier(address verifier) external;
        function setForkVerifier(address verifier) external;
    }
}
