// Copyright 2018 Amazon.com, Inc. or its affiliates.  All Rights Reserved.
use std;
use std::collections::LinkedList;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::result;

use rate_limiter::RateLimiterDescription;

type Result<T> = result::Result<T, DriveError>;

/// Errors associated with the operations allowed on a drive.
#[derive(Debug, PartialEq)]
pub enum DriveError {
    /// Cannot open block device due to invalid permissions or path.
    CannotOpenBlockDevice,
    /// The block device ID is invalid.
    InvalidBlockDeviceID,
    /// The block device path is invalid.
    InvalidBlockDevicePath,
    /// The block device path was already used for a different drive.
    BlockDevicePathAlreadyExists,
    /// Cannot update the block device.
    BlockDeviceUpdateFailed,
    /// Cannot perform the requested operation before booting the microVM.
    OperationNotAllowedPreBoot,
    /// Cannot perform the requested operation after booting the microVM.
    UpdateNotAllowedPostBoot,
    /// A root block device was already added.
    RootBlockDeviceAlreadyAdded,
}

impl Display for DriveError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        use self::DriveError::*;
        match *self {
            CannotOpenBlockDevice => {
                write!(f, "Cannot open block device. Invalid permission/path.")
            }
            InvalidBlockDeviceID => write!(f, "Invalid block device ID!"),
            InvalidBlockDevicePath => write!(f, "Invalid block device path!"),
            BlockDevicePathAlreadyExists => write!(
                f,
                "The block device path was already added to a different drive!"
            ),
            BlockDeviceUpdateFailed => write!(f, "The update operation failed!"),
            OperationNotAllowedPreBoot => write!(f, "Operation not allowed pre-boot!"),
            RootBlockDeviceAlreadyAdded => write!(f, "A root block device already exists!"),
            UpdateNotAllowedPostBoot => {
                write!(f, "The update operation is not allowed after boot.")
            }
        }
    }
}

/// Use this structure to set up the Block Device before booting the kernel.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BlockDeviceConfig {
    /// Unique identifier of the drive.
    pub drive_id: String,
    /// Path of the drive.
    pub path_on_host: PathBuf,
    /// If set to true, it makes the current device the root block device.
    /// Setting this flag to true will mount the block device in the
    /// guest under /dev/vda unless the partuuid is present.
    pub is_root_device: bool,
    /// Part-UUID. Represents the unique id of the boot partition of this device. It is
    /// optional and it will be used only if the `is_root_device` field is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partuuid: Option<String>,
    /// If set to true, the drive is opened in read-only mode. Otherwise, the
    /// drive is opened as read-write.
    pub is_read_only: bool,
    /// Rate Limiter for I/O operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limiter: Option<RateLimiterDescription>,
}

impl BlockDeviceConfig {
    /// Returns a reference to the partuuid.
    pub fn get_partuuid(&self) -> Option<&String> {
        self.partuuid.as_ref()
    }

    /// Checks whether the drive had read only permissions.
    pub fn is_read_only(&self) -> bool {
        self.is_read_only
    }

    /// Returns a reference to `path_on_host`.
    pub fn path_on_host(&self) -> &PathBuf {
        &self.path_on_host
    }
}

/// Wrapper for the collection that holds all the Block Devices Configs
pub struct BlockDeviceConfigs {
    /// a Linked List of `BlockDeviceConfig` objects.
    pub config_list: LinkedList<BlockDeviceConfig>,
    has_root_block: bool,
    has_partuuid_root: bool,
    read_only_root: bool,
}

impl BlockDeviceConfigs {
    /// Constructor for the BlockDeviceConfigs. It initializes an empty LinkedList.
    pub fn new() -> BlockDeviceConfigs {
        BlockDeviceConfigs {
            config_list: LinkedList::<BlockDeviceConfig>::new(),
            has_root_block: false,
            has_partuuid_root: false,
            read_only_root: false,
        }
    }

    /// Checks whether any of the added BlockDevice is the root.
    pub fn has_root_block_device(&self) -> bool {
        return self.has_root_block;
    }

    /// Checks whether the root device has read-only permisssions.
    pub fn has_read_only_root(&self) -> bool {
        self.read_only_root
    }

    /// Checks whether the root device is configured using a part UUID.
    pub fn has_partuuid_root(&self) -> bool {
        self.has_partuuid_root
    }

    /// Checks whether a device with the path specified by `drive_path` already exists
    /// in the list.
    fn contains_drive_path(&self, drive_path: PathBuf) -> bool {
        for drive_config in self.config_list.iter() {
            if drive_config.path_on_host == drive_path {
                return true;
            }
        }
        return false;
    }

    /// Checks whether a device with the specified `drive_id` already exists in the list.
    /// TODO: this should be made private. Right now it is used in the vmm to make a decision
    /// about calling insert or add. Instead, we should follow the same interface as
    /// `NetworkInterfaceConfigs` and only have a public interface for insert which would
    /// implement both cases (add and update).
    pub fn contains_drive_id(&self, drive_id: String) -> bool {
        for drive_config in self.config_list.iter() {
            if drive_config.drive_id == drive_id {
                return true;
            }
        }
        return false;
    }

    /// This function adds a Block Device Config to the list. The root block device is always
    /// added to the beginning of the list. Only one root block device can be added.
    /// TODO: make private. See comment from `contains_drive_id`.
    pub fn add(&mut self, block_device_config: BlockDeviceConfig) -> Result<()> {
        // check if the path exists
        if !block_device_config.path_on_host.exists() {
            return Err(DriveError::InvalidBlockDevicePath);
        }

        if self.contains_drive_path(block_device_config.path_on_host.clone()) {
            return Err(DriveError::BlockDevicePathAlreadyExists);
        }

        // check whether the Device Config belongs to a root device
        // we need to satisfy the condition by which a VMM can only have on root device
        if block_device_config.is_root_device {
            if self.has_root_block {
                return Err(DriveError::RootBlockDeviceAlreadyAdded);
            } else {
                self.has_root_block = true;
                self.read_only_root = block_device_config.is_read_only;
                self.has_partuuid_root = block_device_config.partuuid.is_some();
                // Root Device should be the first in the list whether or not PARTUUID is specified
                // in order to avoid bugs in case of switching from partuuid boot scenarios to
                // /dev/vda boot type.
                self.config_list.push_front(block_device_config);
            }
        } else {
            self.config_list.push_back(block_device_config);
        }

        Ok(())
    }

    fn get_root_id(&self) -> Option<String> {
        if !self.has_root_block {
            return None;
        } else {
            for cfg in self.config_list.iter() {
                if cfg.is_root_device {
                    return Some(cfg.drive_id.clone());
                }
            }
        }
        None
    }

    /// Returns the BlockDeviceConfing with the specified `id`.
    /// TODO: make this function private. It is used only in `set_block_device_path`.
    /// It should be the responsibility of the BlockDeviceConfigs list to update the
    /// path of one of its block devices.
    pub fn get_block_device_config(&self, id: &String) -> Result<BlockDeviceConfig> {
        for drive_config in self.config_list.iter() {
            if drive_config.drive_id.eq(id) {
                return Ok(drive_config.clone());
            }
        }
        Err(DriveError::InvalidBlockDeviceID)
    }

    /// This function updates a Block Device Config. The update fails if it would result in two
    /// root block devices. Full updates are allowed via PUT prior to the guest boot. Partial
    /// updates on path_on_host are allowed via PATCH both before and after boot.
    /// TODO: make private. See comment from `contains_drive_id`.
    pub fn update(&mut self, block_device_config: &BlockDeviceConfig) -> Result<()> {
        // Check if the path exists
        if !block_device_config.path_on_host.exists() {
            return Err(DriveError::InvalidBlockDevicePath);
        }

        let root_id = self.get_root_id();
        for cfg in self.config_list.iter_mut() {
            if cfg.drive_id == block_device_config.drive_id {
                if cfg.is_root_device {
                    // Check if the root block device is being updated.
                    self.has_root_block = block_device_config.is_root_device;
                    self.read_only_root =
                        block_device_config.is_root_device && block_device_config.is_read_only;
                    self.has_partuuid_root = block_device_config.partuuid.is_some();
                } else if block_device_config.is_root_device {
                    // Check if a second root block device is being added.
                    if root_id.is_some() {
                        return Err(DriveError::RootBlockDeviceAlreadyAdded);
                    } else {
                        // One of the non-root blocks is becoming root.
                        self.has_root_block = true;
                        self.read_only_root = block_device_config.is_read_only;
                        self.has_partuuid_root = block_device_config.partuuid.is_some();
                    }
                }
                cfg.is_root_device = block_device_config.is_root_device;
                cfg.path_on_host = block_device_config.path_on_host.clone();
                cfg.is_read_only = block_device_config.is_read_only;
                cfg.rate_limiter = block_device_config.rate_limiter.clone();
                cfg.partuuid = block_device_config.partuuid.clone();

                return Ok(());
            }
        }

        Err(DriveError::BlockDeviceUpdateFailed)
    }
}

#[cfg(test)]
mod tests {
    extern crate tempfile;

    use self::tempfile::NamedTempFile;
    use super::*;

    #[test]
    fn test_create_block_devices_configs() {
        let block_devices_configs = BlockDeviceConfigs::new();
        assert_eq!(block_devices_configs.has_root_block_device(), false);
        assert_eq!(block_devices_configs.config_list.len(), 0);
    }

    #[test]
    fn test_add_non_root_block_device() {
        let dummy_file = NamedTempFile::new().unwrap();
        let dummy_path = dummy_file.path().to_path_buf();
        let dummy_id = String::from("1");
        let dummy_block_device = BlockDeviceConfig {
            path_on_host: dummy_path.clone(),
            is_root_device: false,
            partuuid: None,
            is_read_only: false,
            drive_id: dummy_id.clone(),
            rate_limiter: None,
        };

        let mut block_devices_configs = BlockDeviceConfigs::new();
        assert!(
            block_devices_configs
                .add(dummy_block_device.clone())
                .is_ok()
        );

        assert_eq!(block_devices_configs.has_root_block, false);
        assert_eq!(block_devices_configs.config_list.len(), 1);

        let dev_config = block_devices_configs.config_list.iter().next().unwrap();
        assert_eq!(dev_config, &dummy_block_device);
        assert!(block_devices_configs.contains_drive_path(dummy_path));
        assert!(block_devices_configs.contains_drive_id(dummy_id));
    }

    #[test]
    fn test_add_one_root_block_device() {
        let dummy_file = NamedTempFile::new().unwrap();
        let dummy_path = dummy_file.path().to_path_buf();

        let dummy_block_device = BlockDeviceConfig {
            path_on_host: dummy_path,
            is_root_device: true,
            partuuid: None,
            is_read_only: true,
            drive_id: String::from("1"),
            rate_limiter: None,
        };

        let mut block_devices_configs = BlockDeviceConfigs::new();
        assert!(
            block_devices_configs
                .add(dummy_block_device.clone())
                .is_ok()
        );

        assert_eq!(block_devices_configs.has_root_block, true);
        assert_eq!(block_devices_configs.config_list.len(), 1);
        let dev_config = block_devices_configs.config_list.iter().next().unwrap();
        assert_eq!(dev_config, &dummy_block_device);
        assert_eq!(block_devices_configs.has_read_only_root(), true);
    }

    #[test]
    fn test_add_two_root_block_devices_configs() {
        let dummy_file_1 = NamedTempFile::new().unwrap();
        let dummy_path_1 = dummy_file_1.path().to_path_buf();
        let root_block_device_1 = BlockDeviceConfig {
            path_on_host: dummy_path_1,
            is_root_device: true,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("1"),
            rate_limiter: None,
        };

        let dummy_file_2 = NamedTempFile::new().unwrap();
        let dummy_path_2 = dummy_file_2.path().to_path_buf();
        let root_block_device_2 = BlockDeviceConfig {
            path_on_host: dummy_path_2,
            is_root_device: true,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("2"),
            rate_limiter: None,
        };

        let mut block_devices_configs = BlockDeviceConfigs::new();
        assert!(block_devices_configs.add(root_block_device_1).is_ok());
        assert_eq!(
            block_devices_configs.add(root_block_device_2).unwrap_err(),
            DriveError::RootBlockDeviceAlreadyAdded
        );
    }

    #[test]
    // Test BlockDevicesConfigs::add when you first add the root device and then the other devices.
    fn test_add_root_block_device_first() {
        let dummy_file_1 = NamedTempFile::new().unwrap();
        let dummy_path_1 = dummy_file_1.path().to_path_buf();
        let root_block_device = BlockDeviceConfig {
            path_on_host: dummy_path_1,
            is_root_device: true,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("1"),
            rate_limiter: None,
        };

        let dummy_file_2 = NamedTempFile::new().unwrap();
        let dummy_path_2 = dummy_file_2.path().to_path_buf();
        let dummy_block_device_2 = BlockDeviceConfig {
            path_on_host: dummy_path_2,
            is_root_device: false,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("2"),
            rate_limiter: None,
        };

        let dummy_file_3 = NamedTempFile::new().unwrap();
        let dummy_path_3 = dummy_file_3.path().to_path_buf();
        let dummy_block_device_3 = BlockDeviceConfig {
            path_on_host: dummy_path_3,
            is_root_device: false,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("3"),
            rate_limiter: None,
        };

        let mut block_devices_configs = BlockDeviceConfigs::new();
        assert!(block_devices_configs.add(root_block_device.clone()).is_ok());
        assert!(
            block_devices_configs
                .add(dummy_block_device_2.clone())
                .is_ok()
        );
        assert!(
            block_devices_configs
                .add(dummy_block_device_3.clone())
                .is_ok()
        );

        assert_eq!(block_devices_configs.has_root_block_device(), true);
        assert_eq!(block_devices_configs.has_partuuid_root(), false);
        assert_eq!(block_devices_configs.config_list.len(), 3);

        let mut block_dev_iter = block_devices_configs.config_list.iter();
        assert_eq!(block_dev_iter.next().unwrap(), &root_block_device);
        assert_eq!(block_dev_iter.next().unwrap(), &dummy_block_device_2);
        assert_eq!(block_dev_iter.next().unwrap(), &dummy_block_device_3);
    }

    #[test]
    // Test BlockDevicesConfigs::add when you add other devices first and then the root device.
    fn test_root_block_device_add_last() {
        let dummy_file_1 = NamedTempFile::new().unwrap();
        let dummy_path_1 = dummy_file_1.path().to_path_buf();
        let root_block_device = BlockDeviceConfig {
            path_on_host: dummy_path_1.clone(),
            is_root_device: true,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("1"),
            rate_limiter: None,
        };

        let dummy_file_2 = NamedTempFile::new().unwrap();
        let dummy_path_2 = dummy_file_2.path().to_path_buf();
        let dummy_block_device_2 = BlockDeviceConfig {
            path_on_host: dummy_path_2,
            is_root_device: false,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("2"),
            rate_limiter: None,
        };

        let dummy_file_3 = NamedTempFile::new().unwrap();
        let dummy_path_3 = dummy_file_3.path().to_path_buf();
        let dummy_block_device_3 = BlockDeviceConfig {
            path_on_host: dummy_path_3,
            is_root_device: false,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("3"),
            rate_limiter: None,
        };

        let mut block_devices_configs = BlockDeviceConfigs::new();
        assert!(
            block_devices_configs
                .add(dummy_block_device_2.clone())
                .is_ok()
        );
        assert!(
            block_devices_configs
                .add(dummy_block_device_3.clone())
                .is_ok()
        );
        assert!(block_devices_configs.add(root_block_device.clone()).is_ok());

        assert_eq!(block_devices_configs.has_root_block_device(), true);
        assert_eq!(block_devices_configs.has_partuuid_root(), false);
        assert_eq!(block_devices_configs.config_list.len(), 3);

        let mut block_dev_iter = block_devices_configs.config_list.iter();
        // The root device should be first in the list no matter of the order in
        // which the devices were added.
        assert_eq!(block_dev_iter.next().unwrap(), &root_block_device);
        assert_eq!(block_dev_iter.next().unwrap(), &dummy_block_device_2);
        assert_eq!(block_dev_iter.next().unwrap(), &dummy_block_device_3);
    }

    #[test]
    fn test_update() {
        let dummy_file_1 = NamedTempFile::new().unwrap();
        let dummy_path_1 = dummy_file_1.path().to_path_buf();
        let root_block_device = BlockDeviceConfig {
            path_on_host: dummy_path_1.clone(),
            is_root_device: true,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("1"),
            rate_limiter: None,
        };

        let dummy_file_2 = NamedTempFile::new().unwrap();
        let dummy_path_2 = dummy_file_2.path().to_path_buf();
        let mut dummy_block_device_2 = BlockDeviceConfig {
            path_on_host: dummy_path_2.clone(),
            is_root_device: false,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("2"),
            rate_limiter: None,
        };

        let mut block_devices_configs = BlockDeviceConfigs::new();

        // Add 2 block devices
        assert!(block_devices_configs.add(root_block_device.clone()).is_ok());
        assert!(
            block_devices_configs
                .add(dummy_block_device_2.clone())
                .is_ok()
        );

        // Get OK
        assert!(
            block_devices_configs
                .get_block_device_config(&String::from("1"))
                .eq(&Ok(root_block_device))
        );

        // Get with invalid ID
        assert!(
            block_devices_configs
                .get_block_device_config(&String::from("foo"))
                .is_err()
        );

        // Update OK
        dummy_block_device_2.is_read_only = true;
        assert!(block_devices_configs.update(&dummy_block_device_2).is_ok());

        // Update with invalid path
        let dummy_filename_3 = String::from("test_update_3");
        let dummy_path_3 = PathBuf::from(dummy_filename_3.clone());
        dummy_block_device_2.path_on_host = dummy_path_3;
        assert!(block_devices_configs.update(&dummy_block_device_2).is_err());

        // Update with 2 root block devices
        dummy_block_device_2.path_on_host = dummy_path_2.clone();
        dummy_block_device_2.is_root_device = true;
        assert!(block_devices_configs.update(&dummy_block_device_2).is_err());

        // Switch roots and add a PARTUUID for the new one  .
        let root_block_device_old = BlockDeviceConfig {
            path_on_host: dummy_path_1,
            is_root_device: false,
            partuuid: None,
            is_read_only: false,
            drive_id: String::from("1"),
            rate_limiter: None,
        };
        let root_block_device_new = BlockDeviceConfig {
            path_on_host: dummy_path_2,
            is_root_device: true,
            partuuid: Some("0eaa91a0-01".to_string()),
            is_read_only: false,
            drive_id: String::from("2"),
            rate_limiter: None,
        };
        assert!(&block_devices_configs.update(&root_block_device_old).is_ok());
        assert!(&block_devices_configs.update(&root_block_device_new).is_ok());
        assert!(block_devices_configs.has_partuuid_root);
    }
}
