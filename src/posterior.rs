use cardinal_harness::gateway::{
    pairwise_logprob_posterior, truncate_output_logprobs, PairwiseLogprobPosterior,
    PairwisePreferredSide, TokenLogprob,
};
use cardinal_harness::prompts::RATIO_LADDER;

pub const STORED_LOGPROB_ALTERNATIVES: usize = 50;

pub fn derive_pairwise_posterior(
    output_logprobs: Option<&[TokenLogprob]>,
    higher_ranked: PairwisePreferredSide,
    ratio: f64,
) -> Option<PairwiseLogprobPosterior> {
    output_logprobs.and_then(|logprobs| {
        pairwise_logprob_posterior(logprobs, higher_ranked, ratio, RATIO_LADDER)
    })
}

pub fn output_logprobs_json(
    output_logprobs: Option<&[TokenLogprob]>,
) -> Result<Option<serde_json::Value>, serde_json::Error> {
    output_logprobs
        .map(|logprobs| truncate_output_logprobs(logprobs, STORED_LOGPROB_ALTERNATIVES))
        .map(serde_json::to_value)
        .transpose()
}

pub fn pairwise_posterior_json(
    posterior: Option<&PairwiseLogprobPosterior>,
) -> Result<Option<serde_json::Value>, serde_json::Error> {
    posterior.map(serde_json::to_value).transpose()
}

#[cfg(test)]
mod tests {
    use super::{
        derive_pairwise_posterior, output_logprobs_json, pairwise_posterior_json,
        STORED_LOGPROB_ALTERNATIVES,
    };
    use cardinal_harness::gateway::{PairwisePreferredSide, TokenAlternative, TokenLogprob};

    #[test]
    fn derives_pairwise_posterior_from_logprobs() {
        let logprobs = vec![
            TokenLogprob {
                token: "\"A\"".to_string(),
                logprob: -0.1,
                top_alternatives: vec![TokenAlternative {
                    token: "\"B\"".to_string(),
                    logprob: -2.3,
                }],
            },
            TokenLogprob {
                token: "2.5".to_string(),
                logprob: -0.22,
                top_alternatives: vec![TokenAlternative {
                    token: "2.1".to_string(),
                    logprob: -1.61,
                }],
            },
        ];

        let posterior = derive_pairwise_posterior(Some(&logprobs), PairwisePreferredSide::A, 2.5)
            .expect("posterior");
        assert_eq!(posterior.selected_ratio, 2.5);
        assert_eq!(posterior.selected_ratio_bucket.ratio(), 2.5);
        assert!(posterior.ratio_distribution.top_probability() > 0.7);
        assert!(posterior.higher_ranked_distribution.top_probability() > 0.8);
    }

    #[test]
    fn serializes_output_logprobs_slice() {
        let logprobs = vec![TokenLogprob {
            token: "2.5".to_string(),
            logprob: -0.22,
            top_alternatives: vec![TokenAlternative {
                token: "2.1".to_string(),
                logprob: -1.61,
            }],
        }];

        let value = output_logprobs_json(Some(&logprobs))
            .expect("json")
            .expect("value");
        assert!(value.is_array());
    }

    #[test]
    fn truncates_output_logprob_alternatives_to_storage_cap() {
        let top_alternatives = (0..(STORED_LOGPROB_ALTERNATIVES + 7))
            .map(|idx| TokenAlternative {
                token: format!("{idx}"),
                logprob: -(idx as f64),
            })
            .collect();
        let logprobs = vec![TokenLogprob {
            token: "2.5".to_string(),
            logprob: -0.22,
            top_alternatives,
        }];

        let value = output_logprobs_json(Some(&logprobs))
            .expect("json")
            .expect("value");
        let alternatives = value[0]["top_alternatives"]
            .as_array()
            .expect("alternatives array");
        assert_eq!(alternatives.len(), STORED_LOGPROB_ALTERNATIVES);
    }

    #[test]
    fn serializes_structured_pairwise_posterior() {
        let logprobs = vec![
            TokenLogprob {
                token: "\"A\"".to_string(),
                logprob: -0.1,
                top_alternatives: vec![TokenAlternative {
                    token: "\"B\"".to_string(),
                    logprob: -2.3,
                }],
            },
            TokenLogprob {
                token: "2.5".to_string(),
                logprob: -0.22,
                top_alternatives: vec![TokenAlternative {
                    token: "2.1".to_string(),
                    logprob: -1.61,
                }],
            },
        ];

        let posterior = derive_pairwise_posterior(Some(&logprobs), PairwisePreferredSide::A, 2.5)
            .expect("posterior");
        let value = pairwise_posterior_json(Some(&posterior))
            .expect("json")
            .expect("value");

        assert_eq!(value["selected_higher_ranked"], "A");
        assert!(value["higher_ranked_distribution"]["support"].is_array());
        assert!(value["answer_distribution"]["support"].is_array());
        assert!(value["signed_ln_ratio_distribution"]["distribution"]["support"].is_array());
    }
}
