use std::{
    collections::HashMap,
    io,
    io::{BufReader, ErrorKind},
    path::Path,
};

use lin_alg::f64::Vec3;
use pdbtbx::{Format, PDB, ReadOptions, StrictnessLevel};
use rayon::prelude::*;

use crate::{
    bond_inference::{create_bonds, create_hydrogen_bonds},
    docking::prep::DockType,
    element::Element,
    molecule::{Atom, AtomRole, Chain, Molecule, Residue, ResidueType},
    util::mol_center_size,
};

impl Atom {
    pub fn from_pdb(
        atom_pdb: &pdbtbx::Atom,
        atom_i: usize,
        aa_map: &HashMap<usize, ResidueType>,
    ) -> Self {
        let mut residue_type = ResidueType::Other("".to_owned());
        let mut role = None;

        if let Some(res_type) = aa_map.get(&atom_i) {
            role = match res_type {
                ResidueType::AminoAcid(_aa) => Some(AtomRole::from_name(atom_pdb.name())),
                ResidueType::Water => Some(AtomRole::Water),
                _ => None,
            };

            residue_type = res_type.clone();
        }

        Self {
            serial_number: atom_pdb.serial_number(),
            posit: Vec3::new(atom_pdb.x(), atom_pdb.y(), atom_pdb.z()),
            element: Element::from_pdb(atom_pdb.element()),
            name: atom_pdb.name().to_owned(),
            role,
            residue_type,
            hetero: atom_pdb.hetero(),
            occupancy: None,
            temperature_factor: None,
            partial_charge: None,
            dock_type: Some(DockType::from_str(atom_pdb.name())), // Updated later with Donor/Acceptor
        }
    }
}

impl Molecule {
    /// From `pdbtbx`'s format.
    pub fn from_pdb(pdb: &PDB) -> Self {
        // todo: Maybe return the PDB type here, and store that. Also have a way to
        // todo get molecules from it

        // todo: Pdbtbx doesn't implm this yet for CIF.
        for remark in pdb.remarks() {
            // println!("Remark: {remark:?}");
        }

        println!("Loading atoms...");
        let atoms_pdb: Vec<&pdbtbx::Atom> = pdb.par_atoms().collect();

        println!("Gather residues...");
        let res_pdb: Vec<&pdbtbx::Residue> = pdb.par_residues().collect();

        let mut residues: Vec<Residue> = pdb
            .par_residues()
            .map(|res| Residue::from_pdb(res, &atoms_pdb))
            .collect();

        residues.sort_by_key(|r| r.serial_number);

        println!("Setting up chains...");

        let mut chains = Vec::with_capacity(pdb.chain_count());
        for chain_pdb in pdb.chains() {
            // println!("Chain: {chain_pdb:?}");

            let mut chain = Chain {
                id: chain_pdb.id().to_owned(),
                atoms: Vec::with_capacity(chain_pdb.atom_count()),
                residues: Vec::with_capacity(chain_pdb.residue_count()),
                visible: true,
            };

            for atom_c in chain_pdb.atoms() {
                let atom_pdb = atoms_pdb
                    .iter()
                    .enumerate()
                    .find(|(_i, a)| a.serial_number() == atom_c.serial_number());
                if let Some((i, _atom)) = atom_pdb {
                    chain.atoms.push(i);
                }
            }

            // We don't have a way to, using serial numbers alone, using PDBTBX, find which residues are associated with
            // which chain. This method is a bit more indirect, using both serial number, and atom indexes.
            for res_c in chain_pdb.residues() {
                for (i, res) in residues.iter().enumerate() {
                    if res.serial_number == res_c.serial_number() {
                        let atom_sns_chain: Vec<usize> =
                            res_c.atoms().map(|a| a.serial_number()).collect();
                        // let atom_sns_res: Vec<usize> = res.atoms.iter().map(|a| a.serial_number).collect();
                        let mut atom_sns_res = Vec::with_capacity(res.atoms.len());
                        for atom_i in &res.atoms {
                            atom_sns_res.push(atoms_pdb[*atom_i].serial_number());
                        }

                        // println!("Atoms 1: {:?}", atom_sns_chain);
                        // println!("Atoms 2: {:?}\n", atom_sns_res);

                        if atom_sns_chain == atom_sns_res {
                            chain.residues.push(i);
                        }
                    }
                }

                // let res = residues
                //     .iter()
                //     .enumerate()
                //     .find(|(i, r)| r.serial_number == res_c.serial_number() && r.atoms );
                // if let Some((i, _res)) = res {
                //     chain.residues.push(i);
                // }
            }

            // println!("Chain: {}, {:?}", chain.id, chain.residues);

            chains.push(chain);
        }

        println!("Atoms final...");

        // This pre-computation of the AA map is more efficient;
        let mut aa_map = HashMap::new();
        for res in &residues {
            for atom_i in &res.atoms {
                aa_map.insert(*atom_i, res.res_type.clone());
            }
        }

        // todo: This is taking a while.
        let atoms: Vec<Atom> = atoms_pdb
            .into_iter()
            .enumerate()
            .map(|(i, atom)| Atom::from_pdb(atom, i, &aa_map))
            .collect();

        // todo: We use our own bond inference, since most PDBs seem to lack bond information.
        // let mut bonds = Vec::new();
        // for (a0, a1, bond) in pdb.bonds() {
        //     bonds.push((Atom::from_pdb(a0), Atom::from_pdb(a1), bond));
        // }

        Molecule::new(
            pdb.identifier.clone().unwrap_or_default(),
            atoms,
            chains,
            residues,
            // pdb.secondary_structure.clone(),
            None,
            None,
        )
    }
}

impl Residue {
    pub fn from_pdb(res_pdb: &pdbtbx::Residue, atoms_pdb: &[&pdbtbx::Atom]) -> Self {
        let res_name = res_pdb.name().unwrap_or_default();

        let res_type = ResidueType::from_str(res_name);

        let mut res = Residue {
            serial_number: res_pdb.serial_number(),
            res_type,
            atoms: Vec::new(),
            dihedral: None,
        };

        for atom_c in res_pdb.atoms() {
            let atom_pdb = atoms_pdb
                .iter()
                .enumerate()
                .find(|(_i, a)| a.serial_number() == atom_c.serial_number());
            if let Some((i, _atom)) = atom_pdb {
                res.atoms.push(i);
            }
        }

        res
    }
}

/// From a string of a CIF or PDB text file.
pub fn read_pdb(pdb_text: &str) -> io::Result<PDB> {
    let reader = BufReader::new(pdb_text.as_bytes());

    let (pdb, _errors) = ReadOptions::default()
        .set_level(StrictnessLevel::Loose)
        .set_format(Format::Mmcif) // Must be set explicitly if  using read_raw.
        .read_raw(reader)
        .map_err(|e| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("Problem parsing PDB or CIF text: {e:?}"),
            )
        })?;

    Ok(pdb)
}

/// From file
pub fn load_pdb(path: &Path) -> io::Result<PDB> {
    let (pdb, _errors) = ReadOptions::default()
        // At the default strictness level of Medium, we fail to parse a number of files. Medium and Strict
        // ensure closer conformance to the PDB and CIF specs, but many files in the wild do not. Setting
        // loose is required for practical use cases.
        .set_level(StrictnessLevel::Loose)
        .read(path.to_str().unwrap())
        .map_err(|e| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("Problem opening a PDB or CIF file: {e:?}"),
            )
        })?;

    Ok(pdb)
}

/// Save as PDB or CIF format.
pub fn save_pdb(pdb: &mut PDB, path: &Path) -> io::Result<()> {
    // todo: Update the PDB in state with data from the molecule prior to saving.

    pdbtbx::save(
        pdb,
        path.to_str().unwrap_or_default(),
        StrictnessLevel::Loose,
    )
    .map_err(|e| {
        io::Error::new(
            ErrorKind::InvalidData,
            format!("Problem saving a PDB or CIF file: {e:?}"),
        )
    })
}
