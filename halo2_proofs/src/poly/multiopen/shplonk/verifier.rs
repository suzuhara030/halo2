use super::{construct_intermediate_sets, ChallengeU, ChallengeV, ChallengeY};
use crate::arithmetic::{
    eval_polynomial, evaluate_vanishing_polynomial, lagrange_interpolate, CurveAffine, Engine,
    FieldExt, MillerLoopResult, MultiMillerLoop,
};
use crate::poly::{
    commitment::{Params, ParamsVerifier},
    msm::{PairMSM, PreMSM, ProjectiveMSM, MSM},
    multiopen::{CommitmentReference, Query, VerifierQuery},
    {Coeff, Error, Polynomial},
};
use crate::transcript::{EncodedChallenge, TranscriptRead};

use ff::Field;
use group::prime::PrimeCurveAffine;
use group::{Curve, Group};
use rand::rngs::OsRng;
use std::marker::PhantomData;
use subtle::Choice;

/// Verify a multi-opening proof
pub fn verify_proof<
    'r,
    'params: 'r,
    I,
    C: MultiMillerLoop,
    E: EncodedChallenge<C::G1Affine>,
    T: TranscriptRead<C::G1Affine, E>,
>(
    params: &'params ParamsVerifier<C>,
    transcript: &mut T,
    queries: I,
) -> Result<PairMSM<C::G1Affine>, Error>
where
    I: IntoIterator<Item = VerifierQuery<'r, C::G1Affine>> + Clone,
{
    let intermediate_sets = construct_intermediate_sets(queries);
    let (rotation_sets, super_point_set) = (
        intermediate_sets.rotation_sets,
        intermediate_sets.super_point_set,
    );

    let y: ChallengeY<_> = transcript.squeeze_challenge_scalar();
    let v: ChallengeV<_> = transcript.squeeze_challenge_scalar();

    let h1 = transcript.read_point().map_err(|_| Error::SamplingError)?;
    let u: ChallengeU<_> = transcript.squeeze_challenge_scalar();
    let h2 = transcript.read_point().map_err(|_| Error::SamplingError)?;

    let zt_eval = evaluate_vanishing_polynomial(&super_point_set[..], *u);

    let mut outer_msm: PreMSM<C> = PreMSM::new();

    for rotation_set in rotation_sets.iter() {
        let z_i = evaluate_vanishing_polynomial(&rotation_set.diffs[..], *u);

        let mut inner_msm: ProjectiveMSM<C> = ProjectiveMSM::new();
        for commitment_data in rotation_set.commitments.iter() {
            // calculate low degree equivalent
            let r_x = lagrange_interpolate(&rotation_set.points[..], &commitment_data.evals()[..]);
            // soft commit to low degree equivalent
            let r_eval = eval_polynomial(&r_x[..], *u);
            let r = params.g1 * r_eval;

            let inner_contrib = match commitment_data.get() {
                CommitmentReference::Commitment(c) => c.to_curve() - r,
                CommitmentReference::MSM(msm) => msm.eval().to_curve() - r,
            };

            inner_msm.append_term(C::Scalar::one(), inner_contrib);
        }
        inner_msm.combine_with_base(*y);
        inner_msm.scale(z_i);
        outer_msm.add_msm(inner_msm);
    }
    outer_msm.combine_with_base(*v);
    let mut outer_msm = outer_msm.normalize();
    outer_msm.append_term(-zt_eval, h1);
    outer_msm.append_term(*u, h2);

    let mut left = params.empty_msm();
    left.append_term(C::Scalar::one(), h2);

    let mut right = params.empty_msm();
    right.add_msm(&outer_msm);

    Ok(PairMSM::with(left, right))
}

impl<'a, 'b, C: CurveAffine> Query<C::Scalar> for VerifierQuery<'a, C> {
    type Commitment = CommitmentReference<'a, C>;

    fn get_point(&self) -> C::Scalar {
        self.point
    }
    fn get_eval(&self) -> C::Scalar {
        self.eval
    }
    fn get_commitment(&self) -> Self::Commitment {
        self.commitment
    }
}
