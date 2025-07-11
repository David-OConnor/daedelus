//! Contains setup code, including applying forcefield data to our specific
//! atoms.

// Notes to square away the 3 "atom name" / "Amber atom type" / "force field type" keys.
// This guide shows Type 1. https://emleddin.github.io/comp-chem-website/AMBERguide-AMBER-atom-types.html,
//
// Update: "Type 1" = "type_in_res" in our code now. "Type 2" = "ff_type" for AAs, and "Type 3" = "ff_type" for small mols.
//
// Type 1 Examples: "CA", "HA", "CZ", "HB3", "HH22", HZ2", "N", "H", "HG3", "O", "CD", "C", "HG23", "CG", "CB", "CG1", "HE2", "HB3",
// Type 1 Sources: `amino19.lib`, col 0. mmCIF atom coordinate files.
//
// Type 2 Examples:  "HC", "C8", "HC", "H"(both), "XC", "N"(both), "H"(both), "H1", "CT", "OH", "HO", "2C",
// Type 2 Sources: `amino19.lib` (AA/protein partial charges), col 1. `frcmod.ff19SB`. (AA/protein params)
//
// Small Mol/lig:
// Type 3 Examples: "oh", "h1", "ca", "o", "os", "c6", "n3", "c3"
// Type 3 Sources: `.mol2` generated by Amber. (Small mol coordinates and partial charges) `gaff2.dat` (Small molg params)
//
// MOl2 for ligands also have "C10", "O7" etc, which is ambiguous here, and not required, as their params
// use Type 3, which is present. It's clear what to do for ligand
//
// Best guess: Type 1 identifies labels within the residue only. Type 2 (AA) and Type 3 (small mol) are the FF types.

use std::collections::{HashMap, HashSet};

use bio_files::{
    ResidueType,
    amber_params::{ChargeParams, ForceFieldParamsKeyed, MassParams, VdwParams},
};
use itertools::Itertools;
use lin_alg::f64::Vec3;
use na_seq::{AminoAcid, AminoAcidGeneral, AminoAcidProtenationVariant, element::LjTable};

use crate::{
    FfParamSet,
    dynamics::{
        AtomDynamics, CUTOFF, ForceFieldParamsIndexed, MdState, ParamError, SKIN, ambient::SimBox,
    },
    molecule::{Atom, Bond, Residue},
};

/// Build a single lookup table in which ligand-specific parameters
/// (when given) replace or add to the generic ones.
pub fn merge_params(
    generic: &ForceFieldParamsKeyed,
    lig_specific: Option<&ForceFieldParamsKeyed>,
) -> ForceFieldParamsKeyed {
    // Start with a deep copy of the generic parameters.
    let mut merged = generic.clone();

    if let Some(lig) = lig_specific {
        merged.mass.extend(lig.mass.clone());
        // merged.partial_charges.extend(lig.partial_charges.clone());
        merged.van_der_waals.extend(lig.van_der_waals.clone());

        merged.bond.extend(lig.bond.clone());
        merged.angle.extend(lig.angle.clone());
        merged.dihedral.extend(lig.dihedral.clone());
        merged
            .dihedral_improper
            .extend(lig.dihedral_improper.clone());
    }

    merged
}

/// Associate loaded Force field data (e.g. from Amber) into the atom indices used in a specific
/// dynamics sim. This handles combining general and molecule-specific parameter sets, and converting
/// between atom name, and the specific indices of the atoms we're using.
impl ForceFieldParamsIndexed {
    pub fn new(
        params_general: &ForceFieldParamsKeyed,
        params_specific: Option<&ForceFieldParamsKeyed>,
        atoms: &[Atom],
        bonds: &[Bond],
        adjacency_list: &[Vec<usize>],
    ) -> Result<Self, ParamError> {
        let mut result = Self::default();

        let err = || ParamError::new("Atom missing FF type");

        // Combine the two force field sets. When a value is present in both, refer the lig-specific
        // one.
        let params = merge_params(params_general, params_specific);

        for (i, atom) in atoms.iter().enumerate() {
            let err = || ParamError::new(&format!("Atom missing FF type: {atom}"));
            let ff_type = atom.force_field_type.as_ref().ok_or_else(|| err())?;

            // Mass
            if let Some(mass) = params.mass.get(ff_type) {
                result.mass.insert(i, mass.clone());
            } else {
                if ff_type.starts_with("C") {
                    result.mass.insert(i, params.mass.get("C").unwrap().clone());
                    println!("Using C fallback mass for {ff_type}");
                } else if ff_type.starts_with("N") {
                    result.mass.insert(i, params.mass.get("N").unwrap().clone());
                    println!("Using N fallback mass for {ff_type}");
                } else if ff_type.starts_with("O") {
                    result.mass.insert(i, params.mass.get("O").unwrap().clone());
                    println!("Using O fallback mass for {ff_type}");
                } else {
                    // todo: This is not a good way to do it. Fall back to element-derived etc.
                    result.mass.insert(
                        i,
                        MassParams {
                            atom_type: "".to_string(),
                            mass: 12.001, // todo: Not great...
                            comment: None,
                        },
                    );

                    println!("Missing mass params for {ff_type}");

                    // return Err(ParamError::new(&format!(
                    //     "Missing mass params for {ff_type}"
                    // )));
                }
            }

            // if let Some(q) = params.partial_charges.get(ff_type) {
            //     result.partial_charge.insert(i, q.clone());
            // } else {
            //     println!("Missing partial charge for {ff_type}; setting to 0");
            //     result.partial_charge.insert(i, 0.0);
            //     // todo: Set to 0 and warn instead of erroring?
            //     // return Err(ParamError::new(&format!(
            //     //     "Missing partial charge for {ff_type}"
            //     // )))
            // }

            // Lennard-Jones / van der Waals
            if let Some(vdw) = params.van_der_waals.get(ff_type) {
                result.van_der_waals.insert(i, vdw.clone());
            } else {
                if ff_type.starts_with("C") {
                    result
                        .van_der_waals
                        .insert(i, params.van_der_waals.get("C*").unwrap().clone());
                    println!("Using C* fallback VdW for {ff_type}");
                } else if ff_type.starts_with("N") {
                    result
                        .van_der_waals
                        .insert(i, params.van_der_waals.get("N").unwrap().clone());
                    println!("Using N fallback VdW for {ff_type}");
                } else if ff_type.starts_with("O") {
                    result
                        .van_der_waals
                        .insert(i, params.van_der_waals.get("O").unwrap().clone());
                    println!("Using O fallback VdW for {ff_type}");
                } else {
                    println!("Missing Van der Waals params for {ff_type}");
                    // 0. no interaction.
                    // todo: If this is "CG" etc, fall back to other carbon params instead.
                    result.van_der_waals.insert(
                        i,
                        VdwParams {
                            atom_type: "".to_string(),
                            sigma: 0.,
                            eps: 0.,
                        },
                    );
                }

                // return Err(ParamError::new(&format!(
                //     "Missing Van der Waals params for {ff_type}"
                // )));
            }
        }

        // Bonds
        for bond in bonds {
            let (i, j) = (bond.atom_0, bond.atom_1);
            let (type_i, type_j) = (
                atoms[i].force_field_type.as_ref().ok_or_else(|| err())?,
                atoms[j].force_field_type.as_ref().ok_or_else(|| err())?,
            );

            let data = params
                .bond
                .get(&(type_i.clone(), type_j.clone()))
                .or_else(|| params.bond.get(&(type_j.clone(), type_i.clone())))
                .cloned()
                .ok_or_else(|| {
                    ParamError::new(&format!("Missing bond parameters for {type_i}-{type_j}"))
                })?;

            result.bond_stretching.insert((i.min(j), i.max(j)), data);
        }

        // Angles. (Between 3 atoms)
        for (center, neigh) in adjacency_list.iter().enumerate() {
            if neigh.len() < 2 {
                continue;
            }
            for (&i, &k) in neigh.iter().tuple_combinations() {
                let (type_0, type_1, type_2) = (
                    atoms[i].force_field_type.as_ref().ok_or_else(|| err())?,
                    atoms[center]
                        .force_field_type
                        .as_ref()
                        .ok_or_else(|| err())?,
                    atoms[k].force_field_type.as_ref().ok_or_else(|| err())?,
                );

                let data = params
                    .angle
                    .get(&(type_0.clone(), type_1.clone(), type_2.clone()))
                    // Try the other atom order.
                    .or_else(|| {
                        params
                            .angle
                            .get(&(type_2.clone(), type_1.clone(), type_0.clone()))
                    })
                    .cloned()
                    .ok_or_else(|| {
                        ParamError::new(&format!(
                            "Missing valence angle parameters for {type_0}-{type_1}-{type_2}"
                        ))
                    })?;

                result.angle.insert((i, center, k), data);
            }
        }

        // Proper and improper dihedral angles.
        let mut seen = HashSet::<(usize, usize, usize, usize)>::new();

        // for (j, nbr_j) in adjacency_list.iter().enumerate() {
        //     for &k in nbr_j {
        //         if j >= k {
        //             continue; // treat each central bond j-k once
        //         }
        //         for &i in &adjacency_list[j] {
        //             if i == k {
        //                 continue;
        //             }
        //             for &l in &adjacency_list[k] {
        //                 if l == j {
        //                     continue;
        //                 }
        //
        //                 // let idx_key = (i, j, k, l);
        //                 let idx_key = if i < l { (i, j, k, l) } else { (l, k, j, i) };
        //
        //                 if !seen.insert(idx_key) {
        //                     continue; // already handled through another path
        //                 }
        //
        //                 let (type_0, type_1, type_2, type_3) = (
        //                     atoms[i].force_field_type.as_ref().ok_or_else(|| err())?,
        //                     atoms[j].force_field_type.as_ref().ok_or_else(|| err())?,
        //                     atoms[k].force_field_type.as_ref().ok_or_else(|| err())?,
        //                     atoms[l].force_field_type.as_ref().ok_or_else(|| err())?,
        //                 );
        //
        //                 let types = (
        //                     type_0.clone(),
        //                     type_1.clone(),
        //                     type_2.clone(),
        //                     type_3.clone(),
        //                 );
        //
        //                 let data = params.get_dihedral(&types);
        //
        //                 match data {
        //                     Some(d) => {
        //                         let mut dihe = d.clone();
        //                         // Cache the divided barrier height.
        //                         // Pre-divide, to reduce computations later.
        //                         dihe.barrier_height_vn /= dihe.integer_divisor as f32;
        //                         dihe.integer_divisor = 1;
        //
        //                         result.dihedral.insert(idx_key, dihe);
        //                         // result.dihedral.insert(idx_key, Some(dihe));
        //                     }
        //                     None => {
        //
        //                         return Err(ParamError::new(&format!(
        //                             "Missing dihedral parameters for {type_0}-{type_1}-{type_2}-{type_3}"
        //                         )))
        //
        //                         // return Err(ParamError::new(&format!(
        //                         //     "No dihedral parameters for \
        //                         //      {type_i}-{type_j}-{type_k}-{type_l}"
        //                         // )));
        //                     }
        //                 }
        //             }
        //         }
        //     }
        // }

        // ---------------------------
        // 1. Proper dihedrals i-j-k-l
        // ---------------------------
        for (j, nbr_j) in adjacency_list.iter().enumerate() {
            for &k in nbr_j {
                if j >= k {
                    continue;
                } // handle each j-k bond once

                // fan out the two outer atoms
                for &i in adjacency_list[j].iter().filter(|&&x| x != k) {
                    for &l in adjacency_list[k].iter().filter(|&&x| x != j) {
                        if i == l {
                            continue;
                        } // skip self-torsions

                        // canonicalise so (i,l) is always (min,max)
                        let idx_key = if i < l { (i, j, k, l) } else { (l, k, j, i) };
                        if !seen.insert(idx_key) {
                            continue;
                        }

                        // look up FF types
                        let (ti, tj, tk, tl) = (
                            atoms[i].force_field_type.as_ref().ok_or_else(err)?,
                            atoms[j].force_field_type.as_ref().ok_or_else(err)?,
                            atoms[k].force_field_type.as_ref().ok_or_else(err)?,
                            atoms[l].force_field_type.as_ref().ok_or_else(err)?,
                        );

                        if let Some(dihe) = params
                            .get_dihedral(&(ti.clone(), tj.clone(), tk.clone(), tl.clone()), true)
                        {
                            let mut dihe = dihe.clone();
                            // I believe this may be pri-divided.
                            // dihe.barrier_height /= dihe.divider as f32; // pre-divide
                            dihe.divider = 1;
                            result.dihedral.insert(idx_key, dihe);
                        } else {
                            // return Err(ParamError::new(&format!(
                            //     "Missing dihedral parameters for {ti}-{tj}-{tk}-{tl}"
                            // )));
                        }
                    }
                }
            }
        }

        // ---------------------------
        // 2. Improper dihedrals 2-1-3-4
        // ---------------------------
        for (c, satellites) in adjacency_list.iter().enumerate() {
            if satellites.len() < 3 {
                continue;
            }

            // unique unordered triples of neighbours
            for a in 0..satellites.len() - 2 {
                for b in a + 1..satellites.len() - 1 {
                    for d in b + 1..satellites.len() {
                        let (i, k, l) = (satellites[a], satellites[b], satellites[d]);
                        let idx_key = (i, c, k, l); // order is fixed → no swap
                        if !seen.insert(idx_key) {
                            continue;
                        }

                        let (ti, tc, tk, tl) = (
                            atoms[i].force_field_type.as_ref().ok_or_else(err)?,
                            atoms[c].force_field_type.as_ref().ok_or_else(err)?,
                            atoms[k].force_field_type.as_ref().ok_or_else(err)?,
                            atoms[l].force_field_type.as_ref().ok_or_else(err)?,
                        );

                        // fetch parameters (improper torsion)
                        if let Some(mut dihe) = params
                            .get_dihedral(&(ti.clone(), tc.clone(), tk.clone(), tl.clone()), false)
                        {
                            let mut dihe = dihe.clone();

                            // todo: I believe it's already divided ?
                            // dihe.barrier_height /= dihe.divider as f32;
                            dihe.divider = 1;
                            result.dihedral.insert(idx_key, dihe);
                        } else {
                            // return Err(ParamError::new(&format!(
                            //     "Missing improper parameters for {ti}-{tc}-{tk}-{tl}"
                            // )));
                        }
                    }
                }
            }
        }

        Ok(result)
    }
}

impl MdState {
    pub fn new(
        atoms: &[Atom],
        atom_posits: &[Vec3],
        adjacency_list: &[Vec<usize>],
        bonds: &[Bond],
        atoms_static: &[Atom],
        // lj_table: &LjTable,
        ff_params: &FfParamSet,
        residues: &[Residue], // For protein charge LU
    ) -> Result<Self, ParamError> {
        let Some(ff_params_lig_keyed) = &ff_params.lig_general else {
            return Err(ParamError::new("Missing lig general params"));
        };
        let Some(ff_params_prot_keyed) = &ff_params.prot_general else {
            return Err(ParamError::new("Missing prot params general params"));
        };

        // Assign FF type and charge to protein atoms; FF type must be assigned prior to initializing `ForceFieldParamsIndexed`.
        // (Ligand atoms will already have FF type assigned).

        // todo temp!
        let ff_params_keyed_lig_specific = ff_params.lig_specific.get("CPB");

        // Convert FF params from keyed to index-based.
        let ff_params_lig = ForceFieldParamsIndexed::new(
            ff_params_lig_keyed,
            ff_params_keyed_lig_specific,
            atoms,
            bonds,
            adjacency_list,
        )?;

        // This assumes nonbonded interactions only with external atoms; this is fine for
        // rigid protein models, and is how this is currently structured.
        let bonds_static = Vec::new();
        let adj_list_static = Vec::new();

        let ff_params_prot = ForceFieldParamsIndexed::new(
            ff_params_prot_keyed,
            None,
            atoms_static,
            &bonds_static,
            &adj_list_static,
        )?;

        // We are using this approach instead of `.into`, so we can use the atom_posits from
        // the positioned ligand. (its atom coords are relative; we need absolute)
        let mut atoms_dy = Vec::with_capacity(atoms.len());
        for (i, atom) in atoms.iter().enumerate() {
            atoms_dy.push(AtomDynamics::new(atom, atom_posits, &ff_params_lig, i)?);
        }

        let mut atoms_dy_external = Vec::with_capacity(atoms_static.len());
        let atom_posits_external: Vec<_> = atoms_static.iter().map(|a| a.posit).collect();

        // for (i, atom) in atoms_external.iter().enumerate() {
        for (i, atom) in atoms_static.iter().enumerate() {
            atoms_dy_external.push(AtomDynamics::new(
                atom,
                &atom_posits_external,
                &ff_params_prot,
                i,
            )?);
        }

        let cell = {
            let (mut min, mut max) = (Vec3::splat(f64::INFINITY), Vec3::splat(f64::NEG_INFINITY));
            for a in &atoms_dy {
                min = min.min(a.posit);
                max = max.max(a.posit);
            }
            let pad = 15.0; // Å
            let lo = min - Vec3::splat(pad);
            let hi = max + Vec3::splat(pad);

            println!("Initizing sim box. L: {lo} H: {hi}");

            SimBox { lo, hi }
        };

        let mut result = Self {
            atoms: atoms_dy,
            // bonds: bonds_dy,
            adjacency_list: adjacency_list.to_vec(),
            atoms_static: atoms_dy_external,
            // lj_lut: lj_table.clone(),
            cell,
            excluded_pairs: HashSet::new(),
            scaled14_pairs: HashSet::new(),
            force_field_params: ff_params_lig,
            ..Default::default()
        };

        result.build_masks();
        result.build_neighbours();

        Ok(result)
    }

    // todo: Evaluate whtaq this does, and if you keep it, document.
    fn build_masks(&mut self) {
        // Helper to store pairs in canonical (low,high) order
        let mut push = |set: &mut HashSet<(usize, usize)>, i: usize, j: usize| {
            if i < j {
                set.insert((i, j));
            } else {
                set.insert((j, i));
            }
        };

        // 1-2
        for (indices, _) in &self.force_field_params.bond_stretching {
            push(&mut self.excluded_pairs, indices.0, indices.1);
        }

        // 1-3
        for (indices, _) in &self.force_field_params.angle {
            push(&mut self.excluded_pairs, indices.0, indices.2);
        }

        // 1-4
        for (indices, _) in &self.force_field_params.dihedral {
            push(&mut self.scaled14_pairs, indices.0, indices.3);
        }

        // Make sure no 1-4 pair is also in the excluded set
        for p in &self.scaled14_pairs {
            self.excluded_pairs.remove(p);
        }
    }

    /// Build / rebuild Verlet list
    pub fn build_neighbours(&mut self) {
        let cutoff2 = (CUTOFF + SKIN).powi(2);
        self.neighbour = vec![Vec::new(); self.atoms.len()];
        for i in 0..self.atoms.len() - 1 {
            for j in i + 1..self.atoms.len() {
                let dv = self
                    .cell
                    .min_image(self.atoms[j].posit - self.atoms[i].posit);
                if dv.magnitude_squared() < cutoff2 {
                    self.neighbour[i].push(j);
                    self.neighbour[j].push(i);
                }
            }
        }
        // reset displacement tracker
        for a in &mut self.atoms {
            a.vel /* nothing */;
        }
        self.max_disp_sq = 0.0;
    }
}

/// Populate forcefield type, and partial charge.
/// `residues` must be the full set; this is relevant to how we index it.
pub fn populate_ff_and_q(
    atoms: &mut [Atom],
    residues: &[Residue],
    prot_charge: &HashMap<AminoAcidGeneral, Vec<ChargeParams>>,
) -> Result<(), ParamError> {
    for atom in atoms {
        if atom.hetero {
            continue;
        }
        let Some(res_i) = atom.residue else {
            return Err(ParamError::new(&format!("Missing residue: {:?}", atom)));
        };

        let Some(type_in_res) = &atom.type_in_res else {
            return Err(ParamError::new(&format!(
                "Missing type in residue for SN: {}, {}, {:?}",
                atom.serial_number, atom.posit, atom.element
            )));
        };

        let atom_res_type = &residues[res_i].res_type;

        let ResidueType::AminoAcid(aa) = atom_res_type else {
            // e.g. water or other hetero atoms; skip.
            continue;
        };

        // todo: Eventually, determine how to load non-standard AA variants from files; set up your
        // todo state to use those labels. They are available in the params.
        let aa_gen = AminoAcidGeneral::Standard(*aa);

        let charges = match prot_charge.get(&aa_gen) {
            Some(c) => c,
            // A specific workaround to plain "HIS" being absent from amino19.lib (2025.
            // Choose one of "HID", "HIE", "HIP arbitrarily.
            None if aa_gen == AminoAcidGeneral::Standard(AminoAcid::His) => prot_charge
                .get(&AminoAcidGeneral::Variant(AminoAcidProtenationVariant::Hid))
                .ok_or_else(|| ParamError::new("Unable to find AA mapping"))?,
            None => return Err(ParamError::new("Unable to find AA mapping")),
        };

        let mut found = false;

        for charge in charges {
            // todo: Note that we have multiple branches in some case, due to Amber names like
            // todo: "HYP" for variants on AAs for different protenation states. Handle this.
            if &charge.type_in_res == type_in_res {
                atom.force_field_type = Some(charge.ff_type.clone());
                atom.partial_charge = Some(charge.charge);

                found = true;
                break;
            }
        }

        if atom.serial_number == 2212 {
            println!("Charge data for {atom}");
        }

        if !found {
            // todo: This is a workaround for having trouble with H types. LIkely
            // todo when we create them. For now, this meets the intent.

            // eprintln!("Failed to match H type {ff_type}. Falling back to a generic H");
            // if ff_type.starts_with("H") {
            //     for charge in charges {
            //         if &charge.type_in_res == "H" || &charge.type_in_res == "HA" {
            //             atom.partial_charge = Some(charge.charge);
            //             found = true;
            //         }
            //     }
            // }

            eprintln!("Can't find charge for protein atom: {}", atom);
            //  todo temp?
            // return Err(ParamError::new(&format!(
            //     "Can't find charge for protein atom: {:?}",
            //     atom
            // )));
        }
    }

    Ok(())
}
