use ff::Field;
use std::iter;

use super::super::{circuit::Any, ChallengeBeta, ChallengeGamma, ChallengeX};
use super::{Argument, VerifyingKey};
use crate::{
    arithmetic::{CurveAffine, FieldExt},
    plonk::{self, Error},
    poly::{multiopen::VerifierQuery, Rotation},
    transcript::{EncodedChallenge, TranscriptRead},
};

#[derive(Debug)]
pub struct Committed<C: CurveAffine> {
    /// permutation_product_commitments
    pub permutation_product_commitments: Vec<C>,
}

#[derive(Debug)]
pub struct EvaluatedSet<C: CurveAffine> {
    /// permutation_product_commitment
    pub permutation_product_commitment: C,
    /// permutation_product_eval
    pub permutation_product_eval: C::Scalar,
    /// permutation_product_next_eval
    pub permutation_product_next_eval: C::Scalar,
    /// permutation_product_last_eval
    pub permutation_product_last_eval: Option<C::Scalar>,
}

/// xixi
#[derive(Debug)]
pub struct CommonEvaluated<C: CurveAffine> {
    /// permutation_evals
    pub permutation_evals: Vec<C::Scalar>,
}

#[derive(Debug)]
pub struct Evaluated<C: CurveAffine> {
    /// sets
    pub sets: Vec<EvaluatedSet<C>>,
}

impl Argument {
    pub fn read_product_commitments<
        C: CurveAffine,
        E: EncodedChallenge<C>,
        T: TranscriptRead<C, E>,
    >(
        &self,
        vk: &plonk::VerifyingKey<C>,
        transcript: &mut T,
    ) -> Result<Committed<C>, Error> {
        let chunk_len = vk.cs.degree() - 2;

        let permutation_product_commitments = self
            .columns
            .chunks(chunk_len)
            .map(|_| transcript.read_point())
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Committed {
            permutation_product_commitments,
        })
    }
}

impl<C: CurveAffine> VerifyingKey<C> {
    /// evaluate
    pub fn evaluate<E: EncodedChallenge<C>, T: TranscriptRead<C, E>>(
        &self,
        transcript: &mut T,
    ) -> Result<CommonEvaluated<C>, Error> {
        let permutation_evals = self
            .commitments
            .iter()
            .map(|_| transcript.read_scalar())
            .collect::<Result<Vec<_>, _>>()?;

        Ok(CommonEvaluated { permutation_evals })
    }
}

impl<C: CurveAffine> Committed<C> {
    /// evaluate
    pub fn evaluate<E: EncodedChallenge<C>, T: TranscriptRead<C, E>>(
        self,
        transcript: &mut T,
    ) -> Result<Evaluated<C>, Error> {
        let mut sets = vec![];

        let mut iter = self.permutation_product_commitments.into_iter();

        while let Some(permutation_product_commitment) = iter.next() {
            let permutation_product_eval = transcript.read_scalar()?;
            let permutation_product_next_eval = transcript.read_scalar()?;
            let permutation_product_last_eval = if iter.len() > 0 {
                Some(transcript.read_scalar()?)
            } else {
                None
            };

            sets.push(EvaluatedSet {
                permutation_product_commitment,
                permutation_product_eval,
                permutation_product_next_eval,
                permutation_product_last_eval,
            });
        }

        Ok(Evaluated { sets })
    }
}

impl<C: CurveAffine> Evaluated<C> {
    pub(in crate::plonk) fn expressions<'a>(
        &'a self,
        vk: &'a plonk::VerifyingKey<C>,
        p: &'a Argument,
        common: &'a CommonEvaluated<C>,
        advice_evals: &'a [C::Scalar],
        fixed_evals: &'a [C::Scalar],
        instance_evals: &'a [C::Scalar],
        l_0: C::Scalar,
        l_last: C::Scalar,
        l_blind: C::Scalar,
        beta: ChallengeBeta<C>,
        gamma: ChallengeGamma<C>,
        x: ChallengeX<C>,
    ) -> impl Iterator<Item = C::Scalar> + 'a {
        let chunk_len = vk.cs.degree() - 2;
        iter::empty()
            // Enforce only for the first set.
            // l_0(X) * (1 - z_0(X)) = 0
            .chain(
                self.sets.first().map(|first_set| {
                    l_0 * &(C::Scalar::one() - &first_set.permutation_product_eval)
                }),
            )
            // Enforce only for the last set.
            // l_last(X) * (z_l(X)^2 - z_l(X)) = 0
            .chain(self.sets.last().map(|last_set| {
                (last_set.permutation_product_eval.square() - &last_set.permutation_product_eval)
                    * &l_last
            }))
            // Except for the first set, enforce.
            // l_0(X) * (z_i(X) - z_{i-1}(\omega^(last) X)) = 0
            .chain(
                self.sets
                    .iter()
                    .skip(1)
                    .zip(self.sets.iter())
                    .map(|(set, last_set)| {
                        (
                            set.permutation_product_eval,
                            last_set.permutation_product_last_eval.unwrap(),
                        )
                    })
                    .map(move |(set, prev_last)| (set - &prev_last) * &l_0),
            )
            // And for all the sets we enforce:
            // (1 - (l_last(X) + l_blind(X))) * (
            //   z_i(\omega X) \prod (p(X) + \beta s_i(X) + \gamma)
            // - z_i(X) \prod (p(X) + \delta^i \beta X + \gamma)
            // )
            .chain(
                self.sets
                    .iter()
                    .zip(p.columns.chunks(chunk_len))
                    .zip(common.permutation_evals.chunks(chunk_len))
                    .enumerate()
                    .map(move |(chunk_index, ((set, columns), permutation_evals))| {
                        let mut left = set.permutation_product_next_eval;
                        for (eval, permutation_eval) in columns
                            .iter()
                            .map(|&column| match column.column_type() {
                                Any::Advice => {
                                    advice_evals[vk.cs.get_any_query_index(column, Rotation::cur())]
                                }
                                Any::Fixed => {
                                    fixed_evals[vk.cs.get_any_query_index(column, Rotation::cur())]
                                }
                                Any::Instance => {
                                    instance_evals
                                        [vk.cs.get_any_query_index(column, Rotation::cur())]
                                }
                            })
                            .zip(permutation_evals.iter())
                        {
                            left *= &(eval + &(*beta * permutation_eval) + &*gamma);
                        }

                        let mut right = set.permutation_product_eval;
                        let mut current_delta = (*beta * &*x)
                            * &(C::Scalar::DELTA.pow_vartime(&[(chunk_index * chunk_len) as u64]));
                        for eval in columns.iter().map(|&column| match column.column_type() {
                            Any::Advice => {
                                advice_evals[vk.cs.get_any_query_index(column, Rotation::cur())]
                            }
                            Any::Fixed => {
                                fixed_evals[vk.cs.get_any_query_index(column, Rotation::cur())]
                            }
                            Any::Instance => {
                                instance_evals[vk.cs.get_any_query_index(column, Rotation::cur())]
                            }
                        }) {
                            right *= &(eval + &current_delta + &*gamma);
                            current_delta *= &C::Scalar::DELTA;
                        }

                        (left - &right) * (C::Scalar::one() - &(l_last + &l_blind))
                    }),
            )
    }

    pub(in crate::plonk) fn queries<'r>(
        &'r self,
        vk: &'r plonk::VerifyingKey<C>,
        x: ChallengeX<C>,
    ) -> impl Iterator<Item = VerifierQuery<'r, C>> + Clone {
        let blinding_factors = vk.cs.blinding_factors();
        let x_next = vk.domain.rotate_omega(*x, Rotation::next());
        let x_last = vk
            .domain
            .rotate_omega(*x, Rotation(-((blinding_factors + 1) as i32)));

        iter::empty()
            .chain(self.sets.iter().flat_map(move |set| {
                iter::empty()
                    // Open permutation product commitments at x and \omega^{-1} x
                    // Open permutation product commitments at x and \omega x
                    .chain(Some(VerifierQuery::new_commitment(
                        &set.permutation_product_commitment,
                        *x,
                        set.permutation_product_eval,
                    )))
                    .chain(Some(VerifierQuery::new_commitment(
                        &set.permutation_product_commitment,
                        x_next,
                        set.permutation_product_next_eval,
                    )))
            }))
            // Open it at \omega^{last} x for all but the last set
            .chain(self.sets.iter().rev().skip(1).flat_map(move |set| {
                Some(VerifierQuery::new_commitment(
                    &set.permutation_product_commitment,
                    x_last,
                    set.permutation_product_last_eval.unwrap(),
                ))
            }))
    }
}

impl<C: CurveAffine> CommonEvaluated<C> {
    pub(in crate::plonk) fn queries<'r>(
        &'r self,
        vkey: &'r VerifyingKey<C>,
        x: ChallengeX<C>,
    ) -> impl Iterator<Item = VerifierQuery<'r, C>> + Clone {
        // Open permutation commitments for each permutation argument at x
        vkey.commitments
            .iter()
            .zip(self.permutation_evals.iter())
            .map(move |(commitment, &eval)| VerifierQuery::new_commitment(commitment, *x, eval))
    }
}
