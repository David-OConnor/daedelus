//! Contains setup code, including applying forcefield data to our specific
//! atoms.

use std::collections::HashSet;

use bio_files::amber_params::{DihedralData, ForceFieldParamsKeyed};
use itertools::Itertools;
use lin_alg::f64::Vec3;
use na_seq::element::LjTable;

use crate::{
    dynamics::{
        AtomDynamics, CUTOFF, ForceFieldParamsIndexed, MdState, ParamError, SKIN, ambient::SimBox,
    },
    molecule::{Atom, Bond},
};

/// Build a single lookup table in which ligand-specific parameters
/// (when given) replace or add to the generic ones.
fn merge_params(
    generic: &ForceFieldParamsKeyed,
    lig_specific: Option<&ForceFieldParamsKeyed>,
) -> ForceFieldParamsKeyed {
    // Start with a deep copy of the generic parameters.
    let mut merged = generic.clone();

    if let Some(lig) = lig_specific {
        merged.mass.extend(lig.mass.clone());
        merged.partial_charges.extend(lig.partial_charges.clone());
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
        params_lig_specific: Option<&ForceFieldParamsKeyed>,
        atoms: &[Atom],
        bonds: &[Bond],
        adjacency_list: &[Vec<usize>],
    ) -> Result<Self, ParamError> {
        let mut result = Self::default();

        let err = || ParamError::new("Atom missing FF type");

        // Combine the two force field sets. When a value is present in both, refer the lig-specific
        // one.
        let params = merge_params(params_general, params_lig_specific);

        for (i, atom) in atoms.iter().enumerate() {
            let ff_type = atom.force_field_type.as_ref().ok_or_else(|| err())?;

            // Mass
            if let Some(mass) = params.mass.get(ff_type) {
                result.mass.insert(i, mass.clone());
            } else {
                return Err(ParamError::new(&format!(
                    "Missing Van der Waals params for {ff_type}"
                )));
            }

            // Partial charge
            // todo: Add.
            if let Some(q) = params.partial_charges.get(ff_type) {
                result.partial_charge.insert(i, q.clone());
            } else {
                println!("Missing partial charge for {ff_type}; setting to 0");
                result.partial_charge.insert(i, 0.0);
                // todo: Set to 0 and warn instead of erroring?
                // return Err(ParamError::new(&format!(
                //     "Missing partial charge for {ff_type}"
                // )))
            }

            // Lennard-Jones / van der Waals
            if let Some(vdw) = params.van_der_waals.get(ff_type) {
                result.van_der_waals.insert(i, vdw.clone());
            } else {
                return Err(ParamError::new(&format!(
                    "Missing Van der Waals params for {ff_type}"
                )));
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

            result.bond.insert((i.min(j), i.max(j)), data);
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

        for (j, nbr_j) in adjacency_list.iter().enumerate() {
            for &k in nbr_j {
                if j >= k {
                    continue; // treat each central bond j-k once
                }
                for &i in &adjacency_list[j] {
                    if i == k {
                        continue;
                    }
                    for &l in &adjacency_list[k] {
                        if l == j {
                            continue;
                        }

                        let idx_key = (i, j, k, l);
                        if !seen.insert(idx_key) {
                            continue; // already handled through another path
                        }

                        let (type_0, type_1, type_2, type_3) = (
                            atoms[i].force_field_type.as_ref().ok_or_else(|| err())?,
                            atoms[j].force_field_type.as_ref().ok_or_else(|| err())?,
                            atoms[k].force_field_type.as_ref().ok_or_else(|| err())?,
                            atoms[l].force_field_type.as_ref().ok_or_else(|| err())?,
                        );

                        let types = (
                            type_0.clone(),
                            type_1.clone(),
                            type_2.clone(),
                            type_3.clone(),
                        );

                        let data = params.get_dihedral(&types);

                        match data {
                            Some(d) => {
                                let mut dihe = d.clone();
                                // Cache the divided barrier height.
                                // todo: Put in when ready (When the dihe calcs work)
                                // dihe.barrier_height_vn /= dihe.integer_divisor as f32;
                                // dihe.integer_divisor = 1;

                                result.dihedral.insert(idx_key, dihe);
                                // result.dihedral.insert(idx_key, Some(dihe));
                            }
                            None => {

                                return Err(ParamError::new(&format!(
                                    "Missing dihedral parameters for {type_0}-{type_1}-{type_2}-{type_3}"
                                )))

                                // return Err(ParamError::new(&format!(
                                //     "No dihedral parameters for \
                                //      {type_i}-{type_j}-{type_k}-{type_l}"
                                // )));
                            }
                        }
                    }
                }
            }
        }

        // println!("\n\nFF for this ligand: {:?}", result);

        Ok(result)
    }
}

impl MdState {
    pub fn new(
        atoms: &[Atom],
        atom_posits: &[Vec3],
        adjacency_list: &[Vec<usize>],
        bonds: &[Bond],
        atoms_external: &[Atom],
        lj_table: &LjTable,
        ff_params_keyed: &ForceFieldParamsKeyed,
        ff_params_keyed_lig_specific: Option<&ForceFieldParamsKeyed>,
    ) -> Result<Self, ParamError> {
        // Convert FF params from keyed to index-based.
        let force_field_params = ForceFieldParamsIndexed::new(
            ff_params_keyed,
            ff_params_keyed_lig_specific,
            atoms,
            bonds,
            adjacency_list,
        )?;

        // We are using this approach instead of `.into`, so we can use the atom_posits from
        // the positioned ligand. (its atom coords are relative; we need absolute)
        let mut atoms_dy = Vec::with_capacity(atoms.len());
        for (i, atom) in atoms.iter().enumerate() {
            atoms_dy.push(AtomDynamics::new(
                atom,
                atom_posits,
                &force_field_params,
                i,
            )?);
        }

        // let atoms_dy = atoms.iter().map(|a| a.into()).collect();
        // let bonds_dy = bonds.iter().map(|b| b.into()).collect();

        // // todo: Temp on bonds this way until we know how to init r0.
        // let bonds_dy = bonds
        //     .iter()
        //     .map(|b| BondDynamics::from_bond(b, atoms))
        //     .collect();

        let atoms_dy_external: Vec<_> = atoms_external.iter().map(|a| a.into()).collect();

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
            atoms_external: atoms_dy_external,
            // lj_lut: lj_table.clone(),
            cell,
            excluded_pairs: HashSet::new(),
            scaled14_pairs: HashSet::new(),
            force_field_params,
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
        for (indices, _) in &self.force_field_params.bond {
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
