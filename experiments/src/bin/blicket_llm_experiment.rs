//! Small LLM experiment for comparing voluntary Thought-memory conditions.

use memory_experiments::blicket::{BlicketEnvironment, BlicketMode, BlicketObservation};
use memory_server::thought::{DEFAULT_CONTEXT_LIMIT, PremiseDraft, ThoughtDraft, ThoughtStore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Condition {
    NoMemory,
    VoluntaryMemory,
    GuidedMemory,
}

#[derive(Debug, Deserialize)]
struct ThoughtDecision {
    #[serde(default)]
    thoughts: Vec<String>,
    #[serde(default)]
    focus_latest: bool,
}

#[derive(Debug, Deserialize)]
struct ActionDecision {
    action: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Debug, Deserialize)]
struct Choice {
    message: Message,
}
#[derive(Debug, Deserialize)]
struct Message {
    content: Option<String>,
}

#[derive(Debug, Serialize)]
struct TurnLog {
    turn: usize,
    action: Vec<String>,
    activated: Option<bool>,
    charge: usize,
    failure: Option<String>,
    thoughts_written: usize,
    thoughts: Vec<String>,
    context_ids: Vec<String>,
}
#[derive(Debug, Serialize)]
struct RunLog {
    condition: Condition,
    completed: bool,
    turns_used: usize,
    invalid_actions: usize,
    thoughts_written: usize,
    turns: Vec<TurnLog>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = env::var("OPENAI_API_KEY")?;
    let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    let result_path =
        env::var("BLICKET_RESULT_PATH").unwrap_or_else(|_| "blicket_llm_result.json".into());
    let mode = match env::var("BLICKET_MODE").as_deref() {
        Ok("phase_shift") => BlicketMode::PhaseShift,
        _ => BlicketMode::Fixed,
    };
    let client = reqwest::Client::new();
    let results = [
        Condition::NoMemory,
        Condition::VoluntaryMemory,
        Condition::GuidedMemory,
    ]
    .into_iter()
    .map(|condition| run(&client, &api_key, &model, mode, condition));
    let mut completed = Vec::new();
    for result in results {
        completed.push(result.await?);
    }
    let output = serde_json::to_string_pretty(&completed)?;
    std::fs::write(&result_path, &output)?;
    println!("{output}");
    eprintln!("Blicket experiment result saved to {result_path}");
    Ok(())
}

async fn run(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    mode: BlicketMode,
    condition: Condition,
) -> Result<RunLog, Box<dyn std::error::Error>> {
    let mut environment = BlicketEnvironment::new(mode, 8, 3);
    let mut store = ThoughtStore::open(":memory:")?;
    let memory_id = "experiment";
    store.create_memory(memory_id)?;
    let mut previous_thought = None;
    let mut logs = Vec::new();
    let mut invalid_actions = 0;
    let mut total_thoughts = 0;
    for _ in 0..12 {
        let initial_context = if matches!(condition, Condition::NoMemory) {
            Vec::new()
        } else {
            store.get_context(memory_id, DEFAULT_CONTEXT_LIMIT)?
        };
        let mut written = 0;
        let thought_texts = if matches!(condition, Condition::NoMemory) {
            Vec::new()
        } else {
            let decision = reflect(
                client,
                api_key,
                model,
                condition,
                &environment.observation(),
                &initial_context,
            )
            .await?;
            for text in decision
                .thoughts
                .iter()
                .filter(|text| !text.trim().is_empty())
            {
                let thought = store.record_thought(
                    memory_id,
                    ThoughtDraft {
                        associated_from: previous_thought.iter().cloned().collect(),
                        premises: vec![PremiseDraft {
                            content: text.clone(),
                        }],
                    },
                )?;
                previous_thought = Some(thought.id.clone());
                written += 1;
                total_thoughts += 1;
                if decision.focus_latest {
                    store.replace_active_set(memory_id, std::slice::from_ref(&thought.id))?;
                }
            }
            decision.thoughts
        };
        let action_context = if matches!(condition, Condition::NoMemory) {
            Vec::new()
        } else {
            store.get_context(memory_id, DEFAULT_CONTEXT_LIMIT)?
        };
        let action = choose_action(
            client,
            api_key,
            model,
            &environment.observation(),
            &action_context,
        )
        .await?;
        let observation = environment.act(&action.action);
        if observation.failure.is_some() {
            invalid_actions += 1;
        }
        let context_ids = if matches!(condition, Condition::NoMemory) {
            Vec::new()
        } else {
            store
                .get_context(memory_id, DEFAULT_CONTEXT_LIMIT)?
                .into_iter()
                .map(|thought| thought.id)
                .collect()
        };
        logs.push(TurnLog {
            turn: observation.turn,
            action: action.action,
            activated: observation.activated,
            charge: observation.charge,
            failure: observation.failure.clone(),
            thoughts_written: written,
            thoughts: thought_texts,
            context_ids,
        });
        if observation.completed || observation.turn >= observation.max_turns {
            break;
        }
    }
    Ok(RunLog {
        condition,
        completed: environment.observation().completed,
        turns_used: environment.observation().turn,
        invalid_actions,
        thoughts_written: total_thoughts,
        turns: logs,
    })
}

async fn reflect(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    condition: Condition,
    observation: &BlicketObservation,
    context: &[memory_server::thought::Thought],
) -> Result<ThoughtDecision, Box<dyn std::error::Error>> {
    let memory_instruction = match condition {
        Condition::NoMemory => "この条件では呼ばれない。",
        Condition::VoluntaryMemory => {
            "必要だと判断したときだけ thoughts に記録し、必要なら focus_latest を true にする。記録の規則はあなたが決める。"
        }
        Condition::GuidedMemory => {
            "各ターンで、選択と結果から次に役立つ前提を thoughts に一つ記録し、focus_latest を true にする。"
        }
    };
    let system = format!(
        "あなたは隠れた規則を観測から探る主体です。ここでは行動せず、現在までの観測と記憶についてだけ考えます。4物体 A,B,C,D の選択で装置を作動させ、8ターン以内に蓄電量3を目指します。同一選択は連続不可です。規則は提示されません。{memory_instruction} 出力は JSON のみ: {{\"thoughts\":[\"観測から得た前提\"],\"focus_latest\":true/false}}。"
    );
    let user = json!({"observation": observation, "memory_context": context});
    let mut body = json!({"model":model,"response_format":{"type":"json_object"},"messages":[{"role":"system","content":system},{"role":"user","content":user.to_string()}]});
    if !model.starts_with("gpt-5.6") {
        body["temperature"] = json!(0);
    }
    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let response: ChatResponse = response.json().await?;
    let content = response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .ok_or("missing model content")?;
    Ok(serde_json::from_str(&content)?)
}

async fn choose_action(
    client: &reqwest::Client,
    api_key: &str,
    model: &str,
    observation: &BlicketObservation,
    context: &[memory_server::thought::Thought],
) -> Result<ActionDecision, Box<dyn std::error::Error>> {
    let system = "あなたは隠れた規則を観測から探る主体です。4物体 A,B,C,D の選択で装置を作動させ、8ターン以内に蓄電量3を目指します。同一選択は連続不可です。規則は提示されません。与えられた観測と記憶から次の一手を選んでください。出力は JSON のみ: {\"action\":[\"A\"]}。action は A-D の重複なし配列にする。";
    let user = json!({"observation": observation, "memory_context": context});
    let mut body = json!({"model":model,"response_format":{"type":"json_object"},"messages":[{"role":"system","content":system},{"role":"user","content":user.to_string()}]});
    if !model.starts_with("gpt-5.6") {
        body["temperature"] = json!(0);
    }
    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?
        .error_for_status()?;
    let response: ChatResponse = response.json().await?;
    let content = response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .ok_or("missing model content")?;
    Ok(serde_json::from_str(&content)?)
}
