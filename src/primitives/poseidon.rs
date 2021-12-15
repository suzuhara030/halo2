//! The Poseidon algebraic hash function.

use std::array;
use std::convert::TryInto;
use std::fmt;
use std::iter;
use std::marker::PhantomData;

use halo2::arithmetic::FieldExt;

pub(crate) mod fp;
pub(crate) mod fq;
pub(crate) mod grain;
pub(crate) mod mds;

#[cfg(test)]
pub(crate) mod test_vectors;

mod p128pow5t3;
pub use p128pow5t3::P128Pow5T3;

use grain::SboxType;

/// The type used to hold permutation state.
pub(crate) type State<F, const T: usize> = [F; T];

/// The type used to hold sponge rate.
pub(crate) type SpongeRate<F, const RATE: usize> = [Option<F>; RATE];

/// The type used to hold the MDS matrix and its inverse.
pub(crate) type Mds<F, const T: usize> = [[F; T]; T];

/// A specification for a Poseidon permutation.
pub trait Spec<F: FieldExt, const T: usize, const RATE: usize>: fmt::Debug {
    /// The number of full rounds for this specification.
    ///
    /// This must be an even number.
    fn full_rounds() -> usize;

    /// The number of partial rounds for this specification.
    fn partial_rounds() -> usize;

    /// The S-box for this specification.
    fn sbox(val: F) -> F;

    /// Side-loaded index of the first correct and secure MDS that will be generated by
    /// the reference implementation.
    ///
    /// This is used by the default implementation of [`Spec::constants`]. If you are
    /// hard-coding the constants, you may leave this unimplemented.
    fn secure_mds() -> usize;

    /// Generates `(round_constants, mds, mds^-1)` corresponding to this specification.
    fn constants() -> (Vec<[F; T]>, Mds<F, T>, Mds<F, T>) {
        let r_f = Self::full_rounds();
        let r_p = Self::partial_rounds();

        let mut grain = grain::Grain::new(SboxType::Pow, T as u16, r_f as u16, r_p as u16);

        let round_constants = (0..(r_f + r_p))
            .map(|_| {
                let mut rc_row = [F::zero(); T];
                for (rc, value) in rc_row
                    .iter_mut()
                    .zip((0..T).map(|_| grain.next_field_element()))
                {
                    *rc = value;
                }
                rc_row
            })
            .collect();

        let (mds, mds_inv) = mds::generate_mds::<F, T>(&mut grain, Self::secure_mds());

        (round_constants, mds, mds_inv)
    }
}

/// Runs the Poseidon permutation on the given state.
pub(crate) fn permute<F: FieldExt, S: Spec<F, T, RATE>, const T: usize, const RATE: usize>(
    state: &mut State<F, T>,
    mds: &Mds<F, T>,
    round_constants: &[[F; T]],
) {
    let r_f = S::full_rounds() / 2;
    let r_p = S::partial_rounds();

    let apply_mds = |state: &mut State<F, T>| {
        let mut new_state = [F::zero(); T];
        // Matrix multiplication
        #[allow(clippy::needless_range_loop)]
        for i in 0..T {
            for j in 0..T {
                new_state[i] += mds[i][j] * state[j];
            }
        }
        *state = new_state;
    };

    let full_round = |state: &mut State<F, T>, rcs: &[F; T]| {
        for (word, rc) in state.iter_mut().zip(rcs.iter()) {
            *word = S::sbox(*word + rc);
        }
        apply_mds(state);
    };

    let part_round = |state: &mut State<F, T>, rcs: &[F; T]| {
        for (word, rc) in state.iter_mut().zip(rcs.iter()) {
            *word += rc;
        }
        // In a partial round, the S-box is only applied to the first state word.
        state[0] = S::sbox(state[0]);
        apply_mds(state);
    };

    iter::empty()
        .chain(iter::repeat(&full_round as &dyn Fn(&mut State<F, T>, &[F; T])).take(r_f))
        .chain(iter::repeat(&part_round as &dyn Fn(&mut State<F, T>, &[F; T])).take(r_p))
        .chain(iter::repeat(&full_round as &dyn Fn(&mut State<F, T>, &[F; T])).take(r_f))
        .zip(round_constants.iter())
        .fold(state, |state, (round, rcs)| {
            round(state, rcs);
            state
        });
}

fn poseidon_sponge<F: FieldExt, S: Spec<F, T, RATE>, const T: usize, const RATE: usize>(
    state: &mut State<F, T>,
    input: Option<&Absorbing<F, RATE>>,
    mds_matrix: &Mds<F, T>,
    round_constants: &[[F; T]],
) -> Squeezing<F, RATE> {
    if let Some(Absorbing(input)) = input {
        // `Iterator::zip` short-circuits when one iterator completes, so this will only
        // mutate the rate portion of the state.
        for (word, value) in state.iter_mut().zip(input.iter()) {
            *word += value.expect("poseidon_sponge is called with a padded input");
        }
    }

    permute::<F, S, T, RATE>(state, mds_matrix, round_constants);

    let mut output = [None; RATE];
    for (word, value) in output.iter_mut().zip(state.iter()) {
        *word = Some(*value);
    }
    Squeezing(output)
}

/// The state of the `Sponge`.
// TODO: Seal this trait?
pub trait SpongeMode {}

/// The absorbing state of the `Sponge`.
#[derive(Debug)]
pub struct Absorbing<F, const RATE: usize>(pub(crate) SpongeRate<F, RATE>);

/// The squeezing state of the `Sponge`.
#[derive(Debug)]
pub struct Squeezing<F, const RATE: usize>(pub(crate) SpongeRate<F, RATE>);

impl<F, const RATE: usize> SpongeMode for Absorbing<F, RATE> {}
impl<F, const RATE: usize> SpongeMode for Squeezing<F, RATE> {}

impl<F: fmt::Debug, const RATE: usize> Absorbing<F, RATE> {
    pub(crate) fn init_with(val: F) -> Self {
        Self(
            iter::once(Some(val))
                .chain((1..RATE).map(|_| None))
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        )
    }
}

/// A Poseidon sponge.
pub(crate) struct Sponge<
    F: FieldExt,
    S: Spec<F, T, RATE>,
    M: SpongeMode,
    const T: usize,
    const RATE: usize,
> {
    mode: M,
    state: State<F, T>,
    mds_matrix: Mds<F, T>,
    round_constants: Vec<[F; T]>,
    _marker: PhantomData<S>,
}

impl<F: FieldExt, S: Spec<F, T, RATE>, const T: usize, const RATE: usize>
    Sponge<F, S, Absorbing<F, RATE>, T, RATE>
{
    /// Constructs a new sponge for the given Poseidon specification.
    pub(crate) fn new(initial_capacity_element: F) -> Self {
        let (round_constants, mds_matrix, _) = S::constants();

        let mode = Absorbing([None; RATE]);
        let mut state = [F::zero(); T];
        state[RATE] = initial_capacity_element;

        Sponge {
            mode,
            state,
            mds_matrix,
            round_constants,
            _marker: PhantomData::default(),
        }
    }

    /// Absorbs an element into the sponge.
    pub(crate) fn absorb(&mut self, value: F) {
        for entry in self.mode.0.iter_mut() {
            if entry.is_none() {
                *entry = Some(value);
                return;
            }
        }

        // We've already absorbed as many elements as we can
        let _ = poseidon_sponge::<F, S, T, RATE>(
            &mut self.state,
            Some(&self.mode),
            &self.mds_matrix,
            &self.round_constants,
        );
        self.mode = Absorbing::init_with(value);
    }

    /// Transitions the sponge into its squeezing state.
    pub(crate) fn finish_absorbing(mut self) -> Sponge<F, S, Squeezing<F, RATE>, T, RATE> {
        let mode = poseidon_sponge::<F, S, T, RATE>(
            &mut self.state,
            Some(&self.mode),
            &self.mds_matrix,
            &self.round_constants,
        );

        Sponge {
            mode,
            state: self.state,
            mds_matrix: self.mds_matrix,
            round_constants: self.round_constants,
            _marker: PhantomData::default(),
        }
    }
}

impl<F: FieldExt, S: Spec<F, T, RATE>, const T: usize, const RATE: usize>
    Sponge<F, S, Squeezing<F, RATE>, T, RATE>
{
    /// Squeezes an element from the sponge.
    pub(crate) fn squeeze(&mut self) -> F {
        loop {
            for entry in self.mode.0.iter_mut() {
                if let Some(e) = entry.take() {
                    return e;
                }
            }

            // We've already squeezed out all available elements
            self.mode = poseidon_sponge::<F, S, T, RATE>(
                &mut self.state,
                None,
                &self.mds_matrix,
                &self.round_constants,
            );
        }
    }
}

/// A domain in which a Poseidon hash function is being used.
pub trait Domain<F: FieldExt, const RATE: usize> {
    /// Iterator that outputs padding field elements.
    type Padding: IntoIterator<Item = F>;

    /// The name of this domain, for debug formatting purposes.
    fn name() -> String;

    /// The initial capacity element, encoding this domain.
    fn initial_capacity_element() -> F;

    /// Returns the padding to be appended to the input.
    fn padding(input_len: usize) -> Self::Padding;
}

/// A Poseidon hash function used with constant input length.
///
/// Domain specified in section 4.2 of https://eprint.iacr.org/2019/458.pdf
#[derive(Clone, Copy, Debug)]
pub struct ConstantLength<const L: usize>;

impl<F: FieldExt, const RATE: usize, const L: usize> Domain<F, RATE> for ConstantLength<L> {
    type Padding = iter::Take<iter::Repeat<F>>;

    fn name() -> String {
        format!("ConstantLength<{}>", L)
    }

    fn initial_capacity_element() -> F {
        // Capacity value is $length \cdot 2^64 + (o-1)$ where o is the output length.
        // We hard-code an output length of 1.
        F::from_u128((L as u128) << 64)
    }

    fn padding(input_len: usize) -> Self::Padding {
        assert_eq!(input_len, L);
        // For constant-input-length hashing, we pad the input with zeroes to a multiple
        // of RATE. On its own this would not be sponge-compliant padding, but the
        // Poseidon authors encode the constant length into the capacity element, ensuring
        // that inputs of different lengths do not share the same permutation.
        let k = (L + RATE - 1) / RATE;
        iter::repeat(F::zero()).take(k * RATE - L)
    }
}

/// A Poseidon hash function, built around a sponge.
pub struct Hash<
    F: FieldExt,
    S: Spec<F, T, RATE>,
    D: Domain<F, RATE>,
    const T: usize,
    const RATE: usize,
> {
    sponge: Sponge<F, S, Absorbing<F, RATE>, T, RATE>,
    _domain: PhantomData<D>,
}

impl<F: FieldExt, S: Spec<F, T, RATE>, D: Domain<F, RATE>, const T: usize, const RATE: usize>
    fmt::Debug for Hash<F, S, D, T, RATE>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hash")
            .field("width", &T)
            .field("rate", &RATE)
            .field("R_F", &S::full_rounds())
            .field("R_P", &S::partial_rounds())
            .field("domain", &D::name())
            .finish()
    }
}

impl<F: FieldExt, S: Spec<F, T, RATE>, D: Domain<F, RATE>, const T: usize, const RATE: usize>
    Hash<F, S, D, T, RATE>
{
    /// Initializes a new hasher.
    pub fn init() -> Self {
        Hash {
            sponge: Sponge::new(D::initial_capacity_element()),
            _domain: PhantomData::default(),
        }
    }
}

impl<F: FieldExt, S: Spec<F, T, RATE>, const T: usize, const RATE: usize, const L: usize>
    Hash<F, S, ConstantLength<L>, T, RATE>
{
    /// Hashes the given input.
    pub fn hash(mut self, message: [F; L]) -> F {
        for value in
            array::IntoIter::new(message).chain(<ConstantLength<L> as Domain<F, RATE>>::padding(L))
        {
            self.sponge.absorb(value);
        }
        self.sponge.finish_absorbing().squeeze()
    }
}

#[cfg(test)]
mod tests {
    use halo2::arithmetic::FieldExt;
    use pasta_curves::pallas;

    use super::{permute, ConstantLength, Hash, P128Pow5T3 as OrchardNullifier, Spec};

    #[test]
    fn orchard_spec_equivalence() {
        let message = [pallas::Base::from_u64(6), pallas::Base::from_u64(42)];

        let (round_constants, mds, _) = OrchardNullifier::constants();

        let hasher = Hash::<_, OrchardNullifier, ConstantLength<2>, 3, 2>::init();
        let result = hasher.hash(message);

        // The result should be equivalent to just directly applying the permutation and
        // taking the first state element as the output.
        let mut state = [message[0], message[1], pallas::Base::from_u128(2 << 64)];
        permute::<_, OrchardNullifier, 3, 2>(&mut state, &mds, &round_constants);
        assert_eq!(state[0], result);
    }
}
