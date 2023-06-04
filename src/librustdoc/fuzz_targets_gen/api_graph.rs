use crate::clean::{self, types};
use crate::formats::cache::Cache;
use crate::fuzz_targets_gen::api_function::ApiFunction;
use crate::fuzz_targets_gen::api_sequence::{ApiCall, ApiSequence, ParamType};
use crate::fuzz_targets_gen::api_util;
use crate::fuzz_targets_gen::call_type::CallType;
use crate::fuzz_targets_gen::fuzz_type::FuzzableType;
use crate::fuzz_targets_gen::impl_util::FullNameMap;
use crate::fuzz_targets_gen::mod_visibility::ModVisibity;
use crate::fuzz_targets_gen::prelude_type;
use itertools::Itertools;
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use std::time::Duration;

use rand::thread_rng;
use rand::Rng;
use rustc_middle::ty::Visibility;

use super::api_sequence::ReverseApiSequence;
use super::fuzz_type;
//use super::generic_function::GenericFunction;

lazy_static! {
    static ref RANDOM_WALK_STEPS: FxHashMap<&'static str, usize> = {
        let mut m = FxHashMap::default();
        m.insert("regex", 10000);
        m.insert("url", 10000);
        m.insert("time", 10000);
        m
    };
}

lazy_static! {
    static ref CAN_COVER_NODES: FxHashMap<&'static str, usize> = {
        let mut m = FxHashMap::default();
        m.insert("regex", 96);
        m.insert("serde_json", 41);
        m.insert("clap", 66);
        m
    };
}

#[derive(Clone, Debug)]
pub(crate) struct ApiGraph<'a> {
    /// 当前crate的名字
    pub(crate) _crate_name: String,

    /// 当前待测crate里面公开的API
    pub(crate) api_functions: Vec<ApiFunction>,

    /// 在bfs的时候，访问过的API不再访问
    pub(crate) api_functions_visited: Vec<bool>,

    /// 根据函数签名解析出的API依赖关系
    pub(crate) api_dependencies: Vec<ApiDependency>,

    /// 生成的一切可能的API序列
    pub(crate) api_sequences: Vec<ApiSequence>,

    /// DefId到名字的映射
    pub(crate) full_name_map: FullNameMap,

    /// the visibility of mods，to fix the problem of `pub(crate) use`
    pub(crate) mod_visibility: ModVisibity,

    ///暂时不支持的
    //pub(crate) generic_functions: Vec<GenericFunction>,
    pub(crate) functions_with_unsupported_fuzzable_types: FxHashSet<String>,
    pub(crate) cache: &'a Cache,
    //pub(crate) _sequences_of_all_algorithm : FxFxHashMap<GraphTraverseAlgorithm, Vec<ApiSequence>>
}

use core::fmt::Debug;
use std::thread::sleep;

impl Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache").finish()
    }
}

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum GraphTraverseAlgorithm {
    _Default,
    _Bfs,
    _FastBfs,
    _BfsEndPoint,
    _FastBfsEndPoint,
    _RandomWalk,
    _RandomWalkEndPoint,
    _TryDeepBfs,
    _DirectBackwardSearch,
    _UseRealWorld, //当前的方法，使用解析出来的sequence
}

#[allow(dead_code)]
#[derive(Debug, Clone, Hash, Eq, PartialEq, Copy)]
pub(crate) enum ApiType {
    BareFunction,
    GenericFunction, //currently not support now
}

//函数的依赖关系
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub(crate) struct ApiDependency {
    pub(crate) output_fun: (ApiType, usize), //the index of first func
    pub(crate) input_fun: (ApiType, usize),  //the index of second func
    pub(crate) input_param_index: usize,     //参数的索引
    pub(crate) call_type: CallType,          //调用类型
}

impl<'a> ApiGraph<'a> {
    /// 新建一个api_graph
    pub(crate) fn new(_crate_name: &String, cache: &'a Cache) -> Self {
        //let _sequences_of_all_algorithm = FxFxHashMap::default();
        ApiGraph {
            _crate_name: _crate_name.to_owned(),
            api_functions: Vec::new(),
            api_functions_visited: Vec::new(),
            api_dependencies: Vec::new(),
            api_sequences: Vec::new(),
            full_name_map: FullNameMap::new(),
            mod_visibility: ModVisibity::new(_crate_name),
            //generic_functions: Vec::new(),
            functions_with_unsupported_fuzzable_types: FxHashSet::default(),
            cache,
        }
    }

    /// 向api_graph中投入function，包括method和bare function，支持泛型
    pub(crate) fn add_api_function(&mut self, mut api_fun: ApiFunction) {
        /*if api_fun._is_generic_function() {
            let generic_function = GenericFunction::from(api_fun);
            // self.generic_functions.push(generic_function);
        } else*/
        //泛型函数不会单独考虑
        if api_fun.contains_unsupported_fuzzable_type(self.cache, &self.full_name_map) {
            self.functions_with_unsupported_fuzzable_types.insert(api_fun.full_name.clone());
        } else {
            // FIXME:新加入泛型
            //既然支持了泛型函数，就要初始化generic_substitution
            for generic_arg in &api_fun._generics.params {
                //当这个是泛型类型（而不是生命周期等）
                if let types::GenericParamDefKind::Type { .. } = generic_arg.kind {
                    let generic_name = generic_arg.name.to_string();
                    //暂时只支持把泛型替换成i32
                    api_fun
                        .generic_substitutions
                        .insert(generic_name, clean::Type::Primitive(clean::PrimitiveType::I32));
                }
            }
            self.api_functions.push(api_fun);
        }
    }

    /// 遍历到某个mod的时候，添加mod的可见性，为过滤出可见的api做准备
    pub(crate) fn add_mod_visibility(&mut self, mod_name: &String, visibility: &Visibility) {
        self.mod_visibility.add_one_mod(mod_name, visibility);
    }

    /// 根据prelude type和可见性来过滤api
    pub(crate) fn filter_functions(&mut self) {
        self.filter_functions_defined_on_prelude_type();
        self.filter_api_functions_by_mod_visibility();
        for (idx, api) in self.api_functions.iter().enumerate() {
            println!(
                "api_functions[{}]: {}",
                idx,
                api._pretty_print(self.cache, &self.full_name_map)
            )
        }
        println!("filtered api functions contain {} apis", self.api_functions.len());
    }

    /// 过滤api，一些预装类型的function，比如Result...不在我这个crate里，肯定要过滤掉
    pub(crate) fn filter_functions_defined_on_prelude_type(&mut self) {
        let prelude_types = prelude_type::get_all_preluded_type();
        if prelude_types.len() <= 0 {
            return;
        }
        self.api_functions = self
            .api_functions
            .drain(..)
            .filter(|api_function| api_function.is_not_defined_on_prelude_type(&prelude_types))
            .collect();
    }

    /// 过滤api，根据可见性进行过滤，不是pub就过滤掉
    /// FIXME:  是否必要
    pub(crate) fn filter_api_functions_by_mod_visibility(&mut self) {
        if self.mod_visibility.inner.is_empty() {
            panic!("No mod!!!!!!");
        }

        let invisible_mods = self.mod_visibility.get_invisible_mods();

        let mut new_api_functions = Vec::new();

        //遍历api_graph中的所有的api
        for api_func in &self.api_functions {
            let api_func_name = &api_func.full_name;
            let trait_full_path = &api_func._trait_full_path;
            let mut invisible_flag = false;
            for invisible_mod in &invisible_mods {
                // 两种情况下api不可见：
                // 1. crate::m1::m2::api中的某个mod不可见
                // 2. api实现了某个trait，同时trait不可见
                if api_func_name.as_str().starts_with(invisible_mod.as_str()) {
                    invisible_flag = true;
                    break;
                }
                if let Some(trait_full_path) = trait_full_path {
                    if trait_full_path.as_str().starts_with(invisible_mod) {
                        invisible_flag = true;
                        break;
                    }
                }
            }

            // parent所在mod可见
            if !invisible_flag && api_func.visibility.is_public() {
                new_api_functions.push(api_func.clone());
            }
        }
        self.api_functions = new_api_functions;
    }

    pub(crate) fn set_full_name_map(&mut self, full_name_map: &FullNameMap) {
        self.full_name_map = full_name_map.clone();
    }

    ///找到所有可能的依赖关系，存在api_dependencies中，供后续使用
    pub(crate) fn find_all_dependencies(&mut self) {
        println!("find_dependencies");
        self.api_dependencies.clear();

        // 两个api_function之间的dependency
        // 其中i和j分别是first_fun和second_fun在api_graph的index
        for (i, first_fun) in self.api_functions.iter().enumerate() {
            if first_fun._is_end_function(self.cache, &self.full_name_map) {
                //如果第一个函数是终止节点，就不寻找这样的依赖
                continue;
            }

            if let Some(ty_) = &first_fun.output {
                let output_type = ty_;

                for (j, second_fun) in self.api_functions.iter().enumerate() {
                    //FIXME:是否要把i=j的情况去掉？
                    if second_fun._is_start_function(self.cache, &self.full_name_map) {
                        //如果第二个节点是开始节点，那么直接跳过
                        continue;
                    }
                    println!(
                        "\nThe first function {} is: {}",
                        i,
                        first_fun._pretty_print(self.cache, &self.full_name_map)
                    );
                    println!(
                        "The second function {} is: {}",
                        j,
                        second_fun._pretty_print(self.cache, &self.full_name_map)
                    );
                    //FIXME:写一个替换函数，在这里就把type给替换掉。

                    // 下面开始正题
                    // 对于second_fun的每个参数，看看first_fun的返回值是否对应得上
                    for (k, input_type) in second_fun.inputs.iter().enumerate() {
                        //为了添加泛型支持，在这里先替换
                        let output_type = match api_util::substitute_type(
                            output_type.clone(),
                            &first_fun.generic_substitutions,
                        ) {
                            Some(substi) => substi,
                            None => {
                                continue;
                            }
                        };
                        let input_type = match api_util::substitute_type(
                            input_type.clone(),
                            &second_fun.generic_substitutions,
                        ) {
                            Some(substi) => substi,
                            None => {
                                continue;
                            }
                        };

                        println!(
                            "output: {}",
                            api_util::_type_name(&output_type, self.cache, &self.full_name_map)
                                .as_str()
                        );
                        println!(
                            "input: {}",
                            api_util::_type_name(&input_type, self.cache, &self.full_name_map)
                                .as_str()
                        );
                        let call_type = api_util::_same_type(
                            &output_type,
                            &input_type,
                            true,
                            self.cache,
                            &self.full_name_map,
                        );
                        match &call_type {
                            CallType::_NotCompatible => {
                                //如果无法转换，那就算了
                                continue;
                            }
                            _ => {
                                println!("ok, it's ok!!!");
                                //如果可以转换的话，那就存入依赖列表里
                                let one_dependency = ApiDependency {
                                    output_fun: (ApiType::BareFunction, i),
                                    input_fun: (ApiType::BareFunction, j),
                                    input_param_index: k,
                                    call_type: call_type.clone(),
                                };
                                self.api_dependencies.push(one_dependency);
                            }
                        }
                    }
                }
            }
        }

        println!(
            "find_dependencies finished! Num of dependencies is {}.",
            self.api_dependencies.len()
        );
    }

    pub(crate) fn _default_generate_sequences(&mut self, lib_name: &str) {
        //BFS + backward search
        self.generate_all_possoble_sequences(GraphTraverseAlgorithm::_BfsEndPoint, lib_name);
        self._try_to_cover_unvisited_nodes();

        // backward search
        //self.generate_all_possoble_sequences(GraphTraverseAlgorithm::_DirectBackwardSearch);
    }

    pub(crate) fn generate_all_possoble_sequences(
        &mut self,
        algorithm: GraphTraverseAlgorithm,
        lib_name: &str,
    ) {
        //BFS序列的最大长度：即为函数的数量,或者自定义
        //let bfs_max_len = self.api_functions.len();
        let bfs_max_len = 3;
        //random walk的最大步数

        let random_walk_max_size = if RANDOM_WALK_STEPS.contains_key(self._crate_name.as_str()) {
            RANDOM_WALK_STEPS.get(self._crate_name.as_str()).unwrap().clone()
        } else {
            100000
        };

        //no depth bound
        let random_walk_max_depth = 0;
        //try deep sequence number
        let max_sequence_number = 100000;
        match algorithm {
            GraphTraverseAlgorithm::_Bfs => {
                println!("using bfs");
                self.bfs(bfs_max_len, false, false);
            }
            GraphTraverseAlgorithm::_FastBfs => {
                println!("using fastbfs");
                self.bfs(bfs_max_len, false, true);
            }
            GraphTraverseAlgorithm::_BfsEndPoint | GraphTraverseAlgorithm::_Default => {
                println!("using bfs end point");
                self.bfs(bfs_max_len, true, false);
            }
            GraphTraverseAlgorithm::_FastBfsEndPoint => {
                println!("using fast bfs end point");
                self.bfs(bfs_max_len, true, true);
            }
            GraphTraverseAlgorithm::_TryDeepBfs => {
                println!("using try deep bfs");
                self._try_deep_bfs(max_sequence_number);
            }
            GraphTraverseAlgorithm::_RandomWalk => {
                println!("using random walk");
                self.random_walk(random_walk_max_size, false, random_walk_max_depth);
            }
            GraphTraverseAlgorithm::_RandomWalkEndPoint => {
                println!("using random walk end point");
                self.random_walk(random_walk_max_size, true, random_walk_max_depth);
            }

            GraphTraverseAlgorithm::_DirectBackwardSearch => {
                println!("using backward search");
                self.api_sequences.clear();
                self.reset_visited();
                self._try_to_cover_unvisited_nodes();
            }
            GraphTraverseAlgorithm::_UseRealWorld => {
                println!("using realworld to generate");
                self.real_world(lib_name);
            }
        }
    }

    pub(crate) fn reset_visited(&mut self) {
        self.api_functions_visited.clear();
        let api_function_num = self.api_functions.len();
        for _ in 0..api_function_num {
            self.api_functions_visited.push(false);
        }
        //FIXME:还有别的序列可能需要reset
    }

    //检查是否所有函数都访问过了
    pub(crate) fn check_all_visited(&self) -> bool {
        let mut visited_nodes = 0;
        for visited in &self.api_functions_visited {
            if *visited {
                visited_nodes = visited_nodes + 1;
            }
        }

        if CAN_COVER_NODES.contains_key(self._crate_name.as_str()) {
            let to_cover_nodes = CAN_COVER_NODES.get(self._crate_name.as_str()).unwrap().clone();
            if visited_nodes == to_cover_nodes {
                return true;
            } else {
                return false;
            }
        }

        if visited_nodes == self.api_functions_visited.len() {
            return true;
        } else {
            return false;
        }
    }

    //已经访问过的节点数量,用来快速判断bfs是否还需要run下去：如果一轮下来，bfs的长度没有发生变化，那么也可直接quit了
    pub(crate) fn _visited_nodes_num(&self) -> usize {
        let visited: Vec<&bool> =
            (&self.api_functions_visited).into_iter().filter(|x| **x == true).collect();
        visited.len()
    }

    //生成函数序列，且指定调用的参数
    //加入对fast mode的支持
    pub(crate) fn bfs(&mut self, max_len: usize, stop_at_end_function: bool, fast_mode: bool) {
        //清空所有的序列
        //self.api_sequences.clear();
        self.reset_visited();
        if max_len < 1 {
            return;
        }

        let api_function_num = self.api_functions.len();

        //无需加入长度为1的，从空序列开始即可，加入一个长度为0的序列作为初始
        let api_sequence = ApiSequence::new();
        self.api_sequences.push(api_sequence);

        //接下来开始从长度1一直到max_len遍历
        for len in 0..max_len {
            let mut tmp_sequences = Vec::new();
            for sequence in &self.api_sequences {
                if stop_at_end_function && self.is_sequence_ended(sequence) {
                    //如果需要引入终止函数，并且当前序列的最后一个函数是终止函数，那么就不再继续添加
                    continue;
                }
                if sequence.len() == len {
                    tmp_sequences.push(sequence.clone());
                }
            }

            for sequence in &tmp_sequences {
                //长度为len的序列，去匹配每一个函数，如果可以加入的话，就生成一个新的序列
                let api_type = ApiType::BareFunction;
                for api_func_index in 0..api_function_num {
                    //bfs fast, 访问过的函数不再访问
                    if fast_mode && self.api_functions_visited[api_func_index] {
                        continue;
                    }
                    if let Some(new_sequence) =
                        self.is_fun_satisfied(&api_type, api_func_index, sequence)
                    {
                        self.api_sequences.push(new_sequence);
                        self.api_functions_visited[api_func_index] = true;

                        //bfs fast，如果都已经别访问过，直接退出
                        if self.check_all_visited() {
                            //println!("bfs all visited");
                            //return;
                        }
                    }
                }
            }
        }

        println!("There are total {} sequences after bfs", self.api_sequences.len());
        /*if !stop_at_end_function {
            std::process::exit(0);
        }*/
    }

    //为探索比较深的路径专门进行优化
    //主要还是针对比较大的库,函数比较多的
    pub(crate) fn _try_deep_bfs(&mut self, max_sequence_number: usize) {
        //清空所有的序列
        self.api_sequences.clear();
        self.reset_visited();
        let max_len = self.api_functions.len();
        if max_len < 1 {
            return;
        }

        let api_function_num = self.api_functions.len();

        //无需加入长度为1的，从空序列开始即可，加入一个长度为0的序列作为初始
        let api_sequence = ApiSequence::new();
        self.api_sequences.push(api_sequence);

        let mut already_covered_nodes = FxHashSet::default();
        let mut already_covered_edges = FxHashSet::default();
        //接下来开始从长度1一直到max_len遍历
        for len in 0..max_len {
            let current_sequence_number = self.api_sequences.len();
            let covered_nodes = self._visited_nodes_num();
            let mut has_new_coverage_flag = false;
            if len > 2 && current_sequence_number * covered_nodes >= max_sequence_number {
                break;
            }

            let mut tmp_sequences = Vec::new();
            for sequence in &self.api_sequences {
                if self.is_sequence_ended(sequence) {
                    //如果需要引入终止函数，并且当前序列的最后一个函数是终止函数，那么就不再继续添加
                    continue;
                }
                if sequence.len() == len {
                    tmp_sequences.push(sequence.clone());
                }
            }
            for sequence in &tmp_sequences {
                //长度为len的序列，去匹配每一个函数，如果可以加入的话，就生成一个新的序列
                let api_type = ApiType::BareFunction;
                for api_func_index in 0..api_function_num {
                    if let Some(new_sequence) =
                        self.is_fun_satisfied(&api_type, api_func_index, sequence)
                    {
                        let covered_nodes = new_sequence._get_contained_api_functions();
                        for covered_node in &covered_nodes {
                            if !already_covered_nodes.contains(covered_node) {
                                already_covered_nodes.insert(*covered_node);
                                has_new_coverage_flag = true;
                            }
                        }

                        let covered_edges = &new_sequence._covered_dependencies;
                        for covered_edge in covered_edges {
                            if !already_covered_edges.contains(covered_edge) {
                                already_covered_edges.insert(*covered_edge);
                                has_new_coverage_flag = true;
                            }
                        }

                        self.api_sequences.push(new_sequence);
                        self.api_functions_visited[api_func_index] = true;
                    }
                }
            }
            if !has_new_coverage_flag {
                println!("forward bfs can not find more.");
                break;
            }
        }
    }

    pub(crate) fn random_walk(
        &mut self,
        max_size: usize,
        stop_at_end_function: bool,
        max_depth: usize,
    ) {
        self.api_sequences.clear();
        self.reset_visited();

        //没有函数的话，直接return
        if self.api_functions.len() <= 0 {
            return;
        }

        //加入一个长度为0的序列
        let api_sequence = ApiSequence::new();
        self.api_sequences.push(api_sequence);

        //start random work
        let function_len = self.api_functions.len();
        let mut rng = thread_rng();

        // max_size是api序列的最大数量
        for i in 0..max_size {
            let current_sequence_len = self.api_sequences.len();
            let chosen_sequence_index = rng.gen_range(0, current_sequence_len);
            let chosen_sequence = &self.api_sequences[chosen_sequence_index];
            //如果需要在终止节点处停止
            if stop_at_end_function && self.is_sequence_ended(&chosen_sequence) {
                continue;
            }

            //如果深度没有很深，就继续加
            if max_depth > 0 && chosen_sequence.len() >= max_depth {
                continue;
            }
            let chosen_fun_index = rng.gen_range(0, function_len);
            //let chosen_fun = &self.api_functions[chosen_fun_index];
            let fun_type = ApiType::BareFunction;
            if let Some(new_sequence) =
                self.is_fun_satisfied(&fun_type, chosen_fun_index, chosen_sequence)
            {
                self.api_sequences.push(new_sequence);
                self.api_functions_visited[chosen_fun_index] = true;

                //如果全都已经访问过，直接退出
                if self.check_all_visited() {
                    println!("random run {} times", i);
                    //return;
                }
            }
        }
    }

    pub(crate) fn real_world(&mut self, lib_name: &str) {
        use std::fs::File;
        use std::io::{BufRead, BufReader};

        let mut sequences = Vec::new();

        //在语料库中所有API
        let mut apis_existing_in_corpus_map = FxHashMap::default();

        let seq_file_path =
            format!("/home/yxz/workspace/fuzz/experiment_root/{}/seq-dedup.ans", lib_name);
        let file = File::open(seq_file_path).unwrap();
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.unwrap();
            let fields = line.split("|").into_iter().map(|x| x.to_string()).collect_vec();

            // 1.解析出序列频率

            let freq = fields.get(1).unwrap();
            let cnt_str: String = freq.chars().filter(|c| c.is_digit(10)).collect();
            let parsed_number: i32 = cnt_str.parse().unwrap();

            // 2.解析sequence

            let sequence = fields.last().unwrap().clone();
            //获得api的名字
            let functions: Vec<String> = sequence
                .split(" ")
                .map(|x| x.to_string())
                .filter(|x| x.len() > 1) //过滤""
                .collect();

            //如果有任何一个在找不到，这个序列被抛弃
            if functions.iter().any(|x| {
                self.api_functions.iter().find(|api| api.full_name.clone() == x.clone()).is_none()
            }) {
                continue;
            }

            for func in functions.clone() {
                if apis_existing_in_corpus_map.contains_key(&func) {
                    //包含这个func，就加上去
                    apis_existing_in_corpus_map.insert(
                        func.clone(),
                        apis_existing_in_corpus_map.get(&func).unwrap() + parsed_number,
                    );
                } else {
                    //如果没有，就创建这个entry
                    apis_existing_in_corpus_map.insert(func, parsed_number);
                }
            }

            sequences.push(functions.clone());

            //打印出名字
            println!("Functions: {:?}", functions);
        }

        // check一下有没有corpus都在里面
        //获得所有存在于corpus里面API的名字
        for (apis, _) in &apis_existing_in_corpus_map {
            if self
                .api_functions
                .iter()
                .find(|x| x.full_name.to_owned() == apis.to_owned())
                .is_none()
            {
                panic!("有corpus的API，在我们这没找到{}。", apis);
            }
        }

        //清空所有的序列
        self.api_sequences.clear();
        self.reset_visited();
        let max_len = 4;
        if max_len < 1 {
            return;
        }

        let mut apis_in_category1 = FxHashMap::default();

        // 对于 Category 1
        for (index, each_sequence) in sequences.iter().enumerate() {
            println!("seq_index = {}, total = {} ", index, sequences.len());

            let mut sequence = ApiSequence::new();
            for func_path in each_sequence {
                //找到图中对应的api
                let f =
                    self.api_functions.iter().enumerate().find(|(_, x)| x.full_name == *func_path);

                match f {
                    Some((api_func_index, func)) => {
                        println!(
                            "find function, name: {}, ",
                            func._pretty_print(self.cache, &self.full_name_map)
                        );

                        let api_type = ApiType::BareFunction;
                        sequence = if let Some(new_sequence) =
                            self.is_fun_satisfied(&api_type, api_func_index, &sequence)
                        {
                            //访问到的api
                            self.api_functions_visited[api_func_index] = true;

                            //apis_existing_in_corpus_map.insert(func_path.clone(), 1);
                            apis_in_category1.insert(func_path, 0);

                            self.api_sequences.push(new_sequence.clone());
                            new_sequence
                        } else {
                            break;
                        };
                    }
                    None => {
                        break;
                    }
                }
            }
        }

        println!("所有被解析出来的function");
        for func in &self.api_functions {
            //println!("{} ", func.full_name);
            println!("{}", func._pretty_print(self.cache, &self.full_name_map));
        }
        println!("打印完了");

        // 对于 Category 2
        let mut apis_in_category2_freq_map = FxHashMap::default();
        //在下面，我们获得category2里面的元素，是String和i32的元组，对应了函数名和频率
        for (api, freq) in &apis_existing_in_corpus_map {
            //如果category1里面不存在，就在category2
            if apis_in_category1.iter().find(|(x1, _)| api.clone() == ***x1.clone()).is_none() {
                apis_in_category2_freq_map.insert(api.clone(), freq);
            }
        }

        println!("Category2: ");
        for (name, _) in &apis_in_category2_freq_map {
            println!("{} ", name);
        }
        println!("");
        if false {
            for (name, _) in &apis_in_category2_freq_map {
                if let Some((tail_api_index, _)) =
                    self.api_functions.iter().enumerate().find(|(_, x)| x.full_name == *name)
                {
                    let mut reverse_seq = match self.reverse_construct(
                        &ApiType::BareFunction,
                        tail_api_index,
                        true,
                    ) {
                        Some(x) => {
                            if x.is_ok(self) {
                                x
                            } else {
                                println!("函数 {} 出错了，无法生成对应序列", name);
                                continue;
                            }
                        }
                        None => {
                            println!("函数 {} 无法生成对应序列", name);
                            continue;
                        }
                    };

                    let api_seq = reverse_seq._generate_api_sequence();
                    self.api_sequences.push(api_seq);
                }
            }
        }

        println!(
            "Total {} functions, {} function exist in corpus, Category1 contains {} apis, Category2 contains {} apis, Category3 contains {} apis",
            self.api_functions.len(),
            apis_existing_in_corpus_map.len(),
            //apis_existing_in_corpus_map.len() - apis_in_category2.len(),
            apis_in_category1.len(),
            apis_in_category2_freq_map.len(),
            self.api_functions.len() - apis_existing_in_corpus_map.len()
        );

        println!("sequences len is {}", self.api_sequences.len());
    }

    pub(crate) fn _choose_candidate_sequence_for_merge(&self) -> Vec<usize> {
        let mut res = Vec::new();
        let all_sequence_number = self.api_sequences.len();
        for i in 0..all_sequence_number {
            let api_sequence = &self.api_sequences[i];
            let dead_code = api_sequence._dead_code(self);
            let api_sequence_len = api_sequence.len();
            if self.is_sequence_ended(api_sequence) {
                //如果当前序列已经结束
                continue;
            }
            if api_sequence_len <= 0 {
                continue;
            } else if api_sequence_len == 1 {
                res.push(i);
            } else {
                let mut dead_code_flag = false;
                for j in 0..api_sequence_len - 1 {
                    if dead_code[j] {
                        dead_code_flag = true;
                        break;
                    }
                }
                if !dead_code_flag {
                    res.push(i);
                }
            }
        }
        res
    }

    pub(crate) fn _try_to_cover_unvisited_nodes(&mut self) {
        //println!("try to cover more nodes");
        let mut apis_covered_by_reverse_search = 0;
        let mut unvisited_nodes = FxHashSet::default();
        let api_fun_number = self.api_functions.len();
        for i in 0..api_fun_number {
            if !self.api_functions_visited[i] {
                unvisited_nodes.insert(i);
            }
        }
        let mut covered_node_this_iteration = FxHashSet::default();
        //最多循环没访问到的节点的数量
        for _ in 0..unvisited_nodes.len() {
            covered_node_this_iteration.clear();
            let candidate_sequences = self._choose_candidate_sequence_for_merge();
            //println!("sequence number, {}", self.api_sequences.len());
            //println!("candidate sequence number, {}", candidate_sequences.len());
            for unvisited_node in &unvisited_nodes {
                let unvisited_api_func = &self.api_functions[*unvisited_node];
                let inputs = &unvisited_api_func.inputs;
                let mut dependent_sequence_indexes = Vec::new();
                let mut can_be_covered_flag = true;
                let input_param_num = inputs.len();
                for i in 0..input_param_num {
                    let input_type = &inputs[i];
                    if api_util::is_fuzzable_type(input_type, self.cache, &self.full_name_map, None)
                    {
                        continue;
                    }
                    let mut can_find_dependency_flag = false;
                    let mut tmp_dependent_index = -1;
                    for candidate_sequence_index in &candidate_sequences {
                        let output_type = ApiType::BareFunction;
                        let input_type = ApiType::BareFunction;
                        let candidate_sequence = &self.api_sequences[*candidate_sequence_index];
                        let output_index = candidate_sequence._last_api_func_index().unwrap();

                        if let Some(_) = self.check_dependency(
                            &output_type,
                            output_index,
                            &input_type,
                            *unvisited_node,
                            i,
                        ) {
                            can_find_dependency_flag = true;
                            //dependent_sequence_indexes.push(*candidate_sequence_index);
                            tmp_dependent_index = *candidate_sequence_index as i32;

                            //prefer sequence with fuzzable inputs
                            if !candidate_sequence._has_no_fuzzables() {
                                break;
                            }
                        }
                    }
                    if !can_find_dependency_flag {
                        can_be_covered_flag = false;
                    } else {
                        dependent_sequence_indexes.push(tmp_dependent_index as usize);
                    }
                }
                if can_be_covered_flag {
                    //println!("{:?} can be covered", unvisited_api_func.full_name);
                    let dependent_sequences: Vec<ApiSequence> = dependent_sequence_indexes
                        .into_iter()
                        .map(|index| self.api_sequences[index].clone())
                        .collect();
                    let merged_sequence = ApiSequence::_merge_sequences(&dependent_sequences);
                    let input_type = ApiType::BareFunction;
                    if let Some(generated_sequence) =
                        self.is_fun_satisfied(&input_type, *unvisited_node, &merged_sequence)
                    {
                        //println!("{}", generated_sequence._to_well_written_function(self, 0, 0));

                        self.api_sequences.push(generated_sequence);
                        self.api_functions_visited[*unvisited_node] = true;
                        covered_node_this_iteration.insert(*unvisited_node);
                        apis_covered_by_reverse_search = apis_covered_by_reverse_search + 1;
                    } else {
                        //The possible cause is there is some wrong fuzzable type
                        println!("Should not go to here. Only if algorithm error occurs");
                    }
                }
            }
            if covered_node_this_iteration.len() == 0 {
                println!("reverse search can not cover more nodes");
                break;
            } else {
                for covered_node in &covered_node_this_iteration {
                    unvisited_nodes.remove(covered_node);
                }
            }
        }

        let mut totol_sequences_number = 0;
        let mut total_length = 0;
        let mut covered_nodes = FxHashSet::default();
        let mut covered_edges = FxHashSet::default();

        for sequence in &self.api_sequences {
            if sequence._has_no_fuzzables() {
                continue;
            }
            totol_sequences_number = totol_sequences_number + 1;
            total_length = total_length + sequence.len();
            let cover_nodes = sequence._get_contained_api_functions();
            for cover_node in &cover_nodes {
                covered_nodes.insert(*cover_node);
            }

            let cover_edges = &sequence._covered_dependencies;
            for cover_edge in cover_edges {
                covered_edges.insert(*cover_edge);
            }
        }

        println!("after backward search");
        println!("targets = {}", totol_sequences_number);
        println!("total length = {}", total_length);
        let average_visit_time = (total_length as f64) / (covered_nodes.len() as f64);
        println!("average time to visit = {}", average_visit_time);
        println!("edge covered by reverse search = {}", covered_edges.len());

        //println!("There are total {} APIs covered by reverse search", apis_covered_by_reverse_search);
    }

    pub(crate) fn _naive_choose_sequence(&self, max_sequence_size: usize) -> Vec<ApiSequence> {
        let mut to_cover_nodes = Vec::new();
        let function_len = self.api_functions.len();
        for i in 0..function_len {
            if self.api_functions_visited[i] {
                to_cover_nodes.push(i);
            }
        }
        let to_cover_nodes_number = to_cover_nodes.len();
        println!("There are total {} nodes need to be covered.", to_cover_nodes_number);

        let mut chosen_sequence_flag = Vec::new();
        let prepared_sequence_number = self.api_sequences.len();
        for _ in 0..prepared_sequence_number {
            chosen_sequence_flag.push(false);
        }

        let mut res = Vec::new();
        let mut node_candidate_sequences = FxHashMap::default();

        for node in &to_cover_nodes {
            node_candidate_sequences.insert(*node, Vec::new());
        }

        for i in 0..prepared_sequence_number {
            let api_sequence = &self.api_sequences[i];
            let contains_nodes = api_sequence._get_contained_api_functions();
            for node in contains_nodes {
                if let Some(v) = node_candidate_sequences.get_mut(&node) {
                    if !v.contains(&i) {
                        v.push(i);
                    }
                }
            }
        }

        let mut rng = thread_rng();
        for _ in 0..max_sequence_size {
            if to_cover_nodes.len() == 0 {
                println!("all {} nodes need to be covered is covered", to_cover_nodes_number);
                break;
            }
            //println!("need_to_cover_nodes:{:?}", to_cover_nodes);
            let next_cover_node = to_cover_nodes.first().unwrap();
            let candidate_sequences =
                node_candidate_sequences.get(next_cover_node).unwrap().clone();
            let unvisited_candidate_sequences = candidate_sequences
                .into_iter()
                .filter(|node| chosen_sequence_flag[*node] == false)
                .collect::<Vec<_>>();
            let candidate_number = unvisited_candidate_sequences.len();
            let random_index = rng.gen_range(0, candidate_number);
            let chosen_index = unvisited_candidate_sequences[random_index];
            //println!("randomc index{}", random_index);
            let chosen_sequence = &self.api_sequences[chosen_index];
            //println!("{:}",chosen_sequence._to_well_written_function(self, 0, 0));

            let covered_nodes = chosen_sequence._get_contained_api_functions();
            to_cover_nodes =
                to_cover_nodes.into_iter().filter(|node| !covered_nodes.contains(node)).collect();
            chosen_sequence_flag[random_index] = true;
            res.push(chosen_sequence.clone());
        }
        res
    }

    pub(crate) fn _random_choose(&self, max_size: usize) -> Vec<ApiSequence> {
        let mut res = Vec::new();
        let mut covered_nodes = FxHashSet::default();
        let mut covered_edges = FxHashSet::default();
        let mut sequence_indexes = Vec::new();

        let total_sequence_size = self.api_sequences.len();

        for i in 0..total_sequence_size {
            sequence_indexes.push(i);
        }

        let mut rng = thread_rng();
        for _ in 0..max_size {
            let rest_sequences_number = sequence_indexes.len();
            if rest_sequences_number <= 0 {
                break;
            }

            let chosen_index = rng.gen_range(0, rest_sequences_number);
            let sequence_index = sequence_indexes[chosen_index];

            let sequence = &self.api_sequences[sequence_index];
            res.push(sequence.clone());
            sequence_indexes.remove(chosen_index);

            for covered_node in sequence._get_contained_api_functions() {
                covered_nodes.insert(covered_node);
            }

            for covered_edge in &sequence._covered_dependencies {
                covered_edges.insert(covered_edge.clone());
            }
        }

        println!("-----------STATISTICS-----------");
        println!("Random selection selected {} targets", res.len());
        println!("Random selection covered {} nodes", covered_nodes.len());
        println!("Random selection covered {} edges", covered_edges.len());
        println!("--------------------------------");

        res
    }

    pub(crate) fn _first_choose(&self, max_size: usize) -> Vec<ApiSequence> {
        let mut res = Vec::new();
        let mut covered_nodes = FxHashSet::default();
        let mut covered_edges = FxHashSet::default();

        let total_sequence_size = self.api_sequences.len();

        for index in 0..total_sequence_size {
            let sequence = &self.api_sequences[index];
            if sequence._has_no_fuzzables() {
                continue;
            }
            res.push(sequence.clone());

            for covered_node in sequence._get_contained_api_functions() {
                covered_nodes.insert(covered_node);
            }

            for covered_edge in &sequence._covered_dependencies {
                covered_edges.insert(covered_edge.clone());
            }

            if res.len() >= max_size {
                break;
            }
        }

        println!("-----------STATISTICS-----------");
        println!("Random walk selected {} targets", res.len());
        println!("Random walk covered {} nodes", covered_nodes.len());
        println!("Random walk covered {} edges", covered_edges.len());
        println!("--------------------------------");

        res
    }

    pub(crate) fn _heuristic_choose(
        &self,
        max_size: usize,
        stop_at_visit_all_nodes: bool,
    ) -> Vec<ApiSequence> {
        let mut res = Vec::new();
        let mut to_cover_nodes = Vec::new();

        let mut fixed_covered_nodes = FxHashSet::default();
        for fixed_sequence in &self.api_sequences {
            //let covered_nodes = fixed_sequence._get_contained_api_functions();
            //for covered_node in &covered_nodes {
            //    fixed_covered_nodes.insert(*covered_node);
            //}

            if !fixed_sequence._has_no_fuzzables()
                && !fixed_sequence._contains_dead_code_except_last_one(self)
            {
                let covered_nodes = fixed_sequence._get_contained_api_functions();
                for covered_node in &covered_nodes {
                    fixed_covered_nodes.insert(*covered_node);
                }
            }
        }

        for fixed_covered_node in fixed_covered_nodes {
            to_cover_nodes.push(fixed_covered_node);
        }

        let to_cover_nodes_number = to_cover_nodes.len();
        //println!("There are total {} nodes need to be covered.", to_cover_nodes_number);
        let to_cover_dependency_number = self.api_dependencies.len();
        //println!("There are total {} edges need to be covered.", to_cover_dependency_number);
        let total_sequence_number = self.api_sequences.len();

        //println!("There are toatl {} sequences.", total_sequence_number);
        let mut valid_fuzz_sequence_count = 0;
        for sequence in &self.api_sequences {
            if !sequence._has_no_fuzzables() && !sequence._contains_dead_code_except_last_one(self)
            {
                valid_fuzz_sequence_count = valid_fuzz_sequence_count + 1;
            }
        }
        //println!("There are toatl {} valid sequences for fuzz.", valid_fuzz_sequence_count);
        if valid_fuzz_sequence_count <= 0 {
            return res;
        }

        let mut already_covered_nodes = FxHashSet::default();
        let mut already_covered_edges = FxHashSet::default();
        let mut already_chosen_sequences = FxHashSet::default();
        let mut sorted_chosen_sequences = Vec::new();
        let mut dynamic_fuzzable_length_sequences_count = 0;
        let mut fixed_fuzzale_length_sequences_count = 0;

        let mut try_to_find_dynamic_length_flag = true;
        for _ in 0..max_size + 1 {
            let mut current_chosen_sequence_index = 0;
            let mut current_max_covered_nodes = 0;
            let mut current_max_covered_edges = 0;
            let mut current_chosen_sequence_len = 0;

            for j in 0..total_sequence_number {
                if already_chosen_sequences.contains(&j) {
                    continue;
                }
                let api_sequence = &self.api_sequences[j];

                if api_sequence._has_no_fuzzables()
                    || api_sequence._contains_dead_code_except_last_one(self)
                {
                    continue;
                }

                if try_to_find_dynamic_length_flag && api_sequence._is_fuzzables_fixed_length() {
                    //优先寻找fuzzable部分具有动态长度的情况
                    continue;
                }

                if !try_to_find_dynamic_length_flag && !api_sequence._is_fuzzables_fixed_length() {
                    //再寻找fuzzable部分具有静态长度的情况
                    continue;
                }

                let covered_nodes = api_sequence._get_contained_api_functions();
                let mut uncovered_nodes_by_former_sequence_count = 0;
                for covered_node in &covered_nodes {
                    if !already_covered_nodes.contains(covered_node) {
                        uncovered_nodes_by_former_sequence_count =
                            uncovered_nodes_by_former_sequence_count + 1;
                    }
                }

                if uncovered_nodes_by_former_sequence_count < current_max_covered_nodes {
                    continue;
                }
                let covered_edges = &api_sequence._covered_dependencies;
                let mut uncovered_edges_by_former_sequence_count = 0;
                for covered_edge in covered_edges {
                    if !already_covered_edges.contains(covered_edge) {
                        uncovered_edges_by_former_sequence_count =
                            uncovered_edges_by_former_sequence_count + 1;
                    }
                }
                if uncovered_nodes_by_former_sequence_count == current_max_covered_nodes
                    && uncovered_edges_by_former_sequence_count < current_max_covered_edges
                {
                    continue;
                }
                let sequence_len = api_sequence.len();
                if (uncovered_nodes_by_former_sequence_count > current_max_covered_nodes)
                    || (uncovered_nodes_by_former_sequence_count == current_max_covered_nodes
                        && uncovered_edges_by_former_sequence_count > current_max_covered_edges)
                    || (uncovered_nodes_by_former_sequence_count == current_max_covered_nodes
                        && uncovered_edges_by_former_sequence_count == current_max_covered_edges
                        && sequence_len < current_chosen_sequence_len)
                {
                    current_chosen_sequence_index = j;
                    current_max_covered_nodes = uncovered_nodes_by_former_sequence_count;
                    current_max_covered_edges = uncovered_edges_by_former_sequence_count;
                    current_chosen_sequence_len = sequence_len;
                }
            }

            if try_to_find_dynamic_length_flag && current_max_covered_nodes <= 0 {
                //println!("sequences with dynamic length can not cover more nodes");
                try_to_find_dynamic_length_flag = false;
                continue;
            }

            if !try_to_find_dynamic_length_flag
                && current_max_covered_edges <= 0
                && current_max_covered_nodes <= 0
            {
                //println!("can't cover more edges or nodes");
                break;
            }
            already_chosen_sequences.insert(current_chosen_sequence_index);
            sorted_chosen_sequences.push(current_chosen_sequence_index);

            if try_to_find_dynamic_length_flag {
                dynamic_fuzzable_length_sequences_count =
                    dynamic_fuzzable_length_sequences_count + 1;
            } else {
                fixed_fuzzale_length_sequences_count = fixed_fuzzale_length_sequences_count + 1;
            }

            let chosen_sequence = &self.api_sequences[current_chosen_sequence_index];

            let covered_nodes = chosen_sequence._get_contained_api_functions();
            for cover_node in covered_nodes {
                already_covered_nodes.insert(cover_node);
            }
            let covered_edges = &chosen_sequence._covered_dependencies;
            //println!("covered_edges = {:?}", covered_edges);
            for cover_edge in covered_edges {
                already_covered_edges.insert(*cover_edge);
            }

            if already_chosen_sequences.len() == valid_fuzz_sequence_count {
                //println!("all sequence visited");
                break;
            }
            if to_cover_dependency_number != 0
                && already_covered_edges.len() == to_cover_dependency_number
            {
                //println!("all edges visited");
                //should we stop at visit all edges?
                break;
            }
            if stop_at_visit_all_nodes && already_covered_nodes.len() == to_cover_nodes_number {
                //println!("all nodes visited");
                break;
            }
            //println!("no fuzzable count = {}", no_fuzzable_count);
        }

        let total_functions_number = self.api_functions.len();
        println!("-----------STATISTICS-----------");
        println!("total nodes: {}", total_functions_number);

        let mut valid_api_number = 0;
        for api_function_ in &self.api_functions {
            if !api_function_.contains_unsupported_fuzzable_type(self.cache, &self.full_name_map) {
                valid_api_number = valid_api_number + 1;
            }
            //else {
            //    println!("{}", api_function_._pretty_print(&self.full_name_map));
            //}
        }
        //println!("total valid nodes: {}", valid_api_number);

        let total_dependencies_number = self.api_dependencies.len();
        println!("total edges: {}", total_dependencies_number);

        let covered_node_num = already_covered_nodes.len();
        let covered_edges_num = already_covered_edges.len();
        println!("covered nodes: {}", covered_node_num);
        println!("covered edges: {}", covered_edges_num);

        let node_coverage = (already_covered_nodes.len() as f64) / (valid_api_number as f64);
        let edge_coverage =
            (already_covered_edges.len() as f64) / (total_dependencies_number as f64);
        println!("node coverage: {}", node_coverage);
        println!("edge coverage: {}", edge_coverage);
        //println!("sequence with dynamic fuzzable length: {}", dynamic_fuzzable_length_sequences_count);
        //println!("sequence with fixed fuzzable length: {}",fixed_fuzzale_length_sequences_count);

        let mut sequnce_covered_by_reverse_search = 0;
        let mut max_length = 0;
        for sequence_index in sorted_chosen_sequences {
            let api_sequence = self.api_sequences[sequence_index].clone();

            if api_sequence.len() > 3 {
                sequnce_covered_by_reverse_search = sequnce_covered_by_reverse_search + 1;
                if api_sequence.len() > max_length {
                    max_length = api_sequence.len();
                }
            }

            res.push(api_sequence);
        }

        println!("targets covered by reverse search: {}", sequnce_covered_by_reverse_search);
        println!("total targets: {}", res.len());
        println!("max length = {}", max_length);

        let mut total_length = 0;
        for selected_sequence in &res {
            total_length = total_length + selected_sequence.len();
        }

        println!("total length = {}", total_length);
        let average_time_to_fuzz_each_api =
            (total_length as f64) / (already_covered_nodes.len() as f64);
        println!("average time to fuzz each api = {}", average_time_to_fuzz_each_api);

        println!("--------------------------------");

        res
    }

    //OK: 判断一个函数能否加入给定的序列中,如果可以加入，返回Some(new_sequence),new_sequence是将新的调用加进去之后的情况，否则返回None
    pub(crate) fn is_fun_satisfied(
        &self,
        input_fun_type: &ApiType, //其实这玩意没用了
        input_fun_index: usize,
        sequence: &ApiSequence,
    ) -> Option<ApiSequence> {
        //判断一个给定的函数能否加入到一个sequence中去
        match input_fun_type {
            ApiType::BareFunction => {
                let mut new_sequence = sequence.clone();
                let mut api_call = ApiCall::_new(input_fun_index);

                let mut _moved_indexes = FxHashSet::default(); //用来保存发生move的那些语句的index
                let mut _multi_mut = FxHashSet::default(); //用来保存会被多次可变引用的情况
                let mut _immutable_borrow = FxHashSet::default(); //不可变借用

                //函数
                let input_function = &self.api_functions[input_fun_index];

                //如果是个unsafe函数，给sequence添加unsafe标记
                if input_function._unsafe_tag._is_unsafe() {
                    new_sequence.set_unsafe();
                }
                //如果用到了trait，添加到序列的trait列表
                if input_function._trait_full_path.is_some() {
                    let trait_full_path = input_function._trait_full_path.as_ref().unwrap();
                    new_sequence.add_trait(trait_full_path);
                }

                //看看之前序列的返回值是否可以作为它的参数
                let input_params = &input_function.inputs;
                if input_params.is_empty() {
                    //无需输入参数，直接是可满足的
                    new_sequence._add_fn(api_call);
                    return Some(new_sequence);
                }
                //对于每个参数进行遍历
                for (i, current_ty) in input_params.iter().enumerate() {
                    // 如果参数是fuzzable的话，...
                    // 在这里T会被替换成concrete type
                    if api_util::is_fuzzable_type(
                        current_ty,
                        self.cache,
                        &self.full_name_map,
                        Some(&input_function.generic_substitutions),
                    ) {
                        /*
                        println!(
                            "param_{} in function {} is fuzzable type",
                            i, input_function.full_name
                        );*/
                        //如果当前参数是fuzzable的
                        let current_fuzzable_index = new_sequence.fuzzable_params.len();
                        let fuzzable_call_type = fuzz_type::fuzzable_call_type(
                            current_ty,
                            self.cache,
                            &self.full_name_map,
                            Some(&input_function.generic_substitutions),
                        );
                        let (fuzzable_type, call_type) =
                            fuzzable_call_type.generate_fuzzable_type_and_call_type();

                        //如果出现了下面这段话，说明出现了Fuzzable参数但不知道如何参数化的
                        //典型例子是tuple里面出现了引用（&usize），这种情况不再去寻找dependency，直接返回无法添加即可
                        match &fuzzable_type {
                            FuzzableType::NoFuzzable => {
                                //println!("Fuzzable Type Error Occurs!");
                                //println!("type = {:?}", current_ty);
                                //println!("fuzzable_call_type = {:?}", fuzzable_call_type);
                                //println!("fuzzable_type = {:?}", fuzzable_type);
                                return None;
                            }
                            _ => {}
                        }

                        //判断要不要加mut tag
                        if api_util::_need_mut_tag(&call_type) {
                            new_sequence._insert_fuzzable_mut_tag(current_fuzzable_index);
                        }

                        //添加到sequence中去
                        new_sequence.fuzzable_params.push(fuzzable_type);
                        api_call._add_param(
                            ParamType::_FuzzableType,
                            current_fuzzable_index,
                            call_type,
                        );
                    }
                    //如果参数不是fuzzable的话，也就是无法直接被afl转化，就需要看看有没有依赖关系
                    else {
                        // 如果当前参数不是fuzzable的，那么就去api sequence寻找是否有这个依赖
                        // 也就是说，api sequence里是否有某个api的返回值是它的参数

                        /*println!(
                            "param_{} in function {} is struct like type",
                            i, input_function.full_name
                        );*/

                        //FIXME: 处理move的情况
                        let functions_in_sequence_len = sequence.functions.len();
                        let mut dependency_flag = false;

                        for function_index in 0..functions_in_sequence_len {
                            // 如果这个sequence里面的该函数返回值已经被move掉了，那么就跳过，不再能被使用了
                            // 后面的都是默认这个返回值没有被move，而是被可变借用或不可变借用
                            if new_sequence._is_moved(function_index)
                                || _moved_indexes.contains(&function_index)
                            {
                                continue;
                            }

                            let found_function = &new_sequence.functions[function_index];
                            let (api_type, index) = &found_function.func;
                            if let Some(dependency_index) = self.check_dependency(
                                api_type,
                                *index,
                                input_fun_type,
                                input_fun_index,
                                i,
                            ) {
                                // 理论上这里泛型依赖也会出现

                                let dependency_ = self.api_dependencies[dependency_index].clone();
                                //将覆盖到的边加入到新的sequence中去
                                new_sequence._add_dependency(dependency_index);
                                //找到了依赖，当前参数是可以被满足的，设置flag并退出循环
                                dependency_flag = true;

                                //如果满足move发生的条件
                                if api_util::_move_condition(current_ty, &dependency_.call_type) {
                                    if _multi_mut.contains(&function_index)
                                        || _immutable_borrow.contains(&function_index)
                                    {
                                        dependency_flag = false;
                                        continue;
                                    } else {
                                        _moved_indexes.insert(function_index);
                                    }
                                }
                                //如果当前调用是可变借用
                                if api_util::_is_mutable_borrow_occurs(
                                    current_ty,
                                    &dependency_.call_type,
                                ) {
                                    //如果之前已经被借用过了
                                    if _multi_mut.contains(&function_index)
                                        || _immutable_borrow.contains(&function_index)
                                    {
                                        dependency_flag = false;
                                        continue;
                                    } else {
                                        _multi_mut.insert(function_index);
                                    }
                                }
                                //如果当前调用是引用，且之前已经被可变引用过，那么这个引用是非法的
                                if api_util::_is_immutable_borrow_occurs(
                                    current_ty,
                                    &dependency_.call_type,
                                ) {
                                    if _multi_mut.contains(&function_index) {
                                        dependency_flag = false;
                                        continue;
                                    } else {
                                        _immutable_borrow.insert(function_index);
                                    }
                                }
                                //参数需要加mut 标记的话
                                if api_util::_need_mut_tag(&dependency_.call_type) {
                                    new_sequence._insert_function_mut_tag(function_index);
                                }
                                //如果call type是unsafe的，那么给sequence加上unsafe标记
                                if dependency_.call_type.unsafe_call_type()._is_unsafe() {
                                    new_sequence.set_unsafe();
                                }
                                api_call._add_param(
                                    ParamType::_FunctionReturn,
                                    function_index,
                                    dependency_.call_type,
                                );
                                break;
                            }
                        }
                        if !dependency_flag {
                            //如果这个参数没有寻找到依赖，则这个函数不可以被加入到序列中
                            return None;
                        }
                    }
                }
                //所有参数都可以找到依赖，那么这个函数就可以加入序列
                new_sequence._add_fn(api_call);
                for move_index in _moved_indexes {
                    new_sequence._insert_move_index(move_index);
                }
                if new_sequence._contains_multi_dynamic_length_fuzzable() {
                    //如果新生成的序列包含多维可变的参数，就不把这个序列加进去
                    return None;
                }
                return Some(new_sequence);
            }
            ApiType::GenericFunction => None,
        }
    }

    /// 从后往前推，做一个dfs
    pub(crate) fn reverse_construct(
        &self,
        tail_api_type: &ApiType,
        tail_api_index: usize,
        print: bool,
    ) -> Option<ReverseApiSequence> {
        match tail_api_type {
            ApiType::BareFunction => {
                if print {
                    println!("开始反向构造");
                }
                //初始化新反向序列
                let mut new_reverse_sequence = ReverseApiSequence::new();

                //let mut _moved_indexes = FxHashSet::default(); //用来保存发生move的那些语句的index
                //let mut _multi_mut = FxHashSet::default(); //用来保存会被多次可变引用的情况
                //let mut _immutable_borrow = FxHashSet::default(); //不可变借用

                //我们为终止API创建了调用点，然后要在其中加入api_call
                let mut api_call = ApiCall::_new(tail_api_index);

                let (_, input_fun_index) = api_call.func;
                let input_fun = &self.api_functions[input_fun_index];
                let params = &input_fun.inputs;

                println!("name: {}", input_fun.full_name);
                sleep(Duration::from_millis(20));

                //对于当前函数的param，有依赖
                let mut param_reverse_sequences = Vec::new();
                let mut current_param_index = 1;

                //对每个都要找个参数
                for (input_param_index_, current_ty) in params.iter().enumerate() {
                    /*********************************************************************************************************/
                    //如果当前参数是可fuzz的
                    if api_util::is_fuzzable_type(current_ty, self.cache, &self.full_name_map, None)
                    {
                        //如果当前参数是fuzzable的
                        let current_fuzzable_index = new_reverse_sequence.fuzzable_params.len();
                        let fuzzable_call_type = fuzz_type::fuzzable_call_type(
                            current_ty,
                            self.cache,
                            &self.full_name_map,
                            None,
                        );
                        let (fuzzable_type, call_type) =
                            fuzzable_call_type.generate_fuzzable_type_and_call_type();

                        //如果出现了下面这段话，说明出现了Fuzzable参数但不知道如何参数化的
                        //典型例子是tuple里面出现了引用（&usize），这种情况不再去寻找dependency，直接返回无法添加即可
                        match &fuzzable_type {
                            FuzzableType::NoFuzzable => {
                                return None;
                            }
                            _ => {}
                        }

                        //判断要不要加mut tag
                        if api_util::_need_mut_tag(&call_type) {
                            new_reverse_sequence._insert_fuzzable_mut_tag(current_fuzzable_index);
                        }

                        //添加到sequence中去
                        new_reverse_sequence.fuzzable_params.push(fuzzable_type);
                        api_call._add_param(
                            ParamType::_FuzzableType,
                            current_fuzzable_index,
                            call_type,
                        );
                    }
                    /******************************************************************************************************** */
                    //如果当前参数不可由afl提供，只能去找依赖
                    else {
                        let mut dependency_flag = false;
                        //遍历函数，看看哪个函数的output可以作为当前的param
                        for (output_fun_index, _output_fun) in self.api_functions.iter().enumerate()
                        {
                            //防止死循环
                            if output_fun_index == input_fun_index {
                                break;
                            }

                            //检查前后是否有依赖关系
                            //output_fun -> struct -> input_fun
                            if let Some(dependency_index) = self.check_dependency(
                                &ApiType::BareFunction,
                                output_fun_index,
                                &api_call.func.0,
                                input_fun_index,
                                input_param_index_,
                            ) {
                                let param_seq = match self.reverse_construct(
                                    &ApiType::BareFunction,
                                    output_fun_index,
                                    false,
                                ) {
                                    Some(seq) => seq,
                                    None => {
                                        //没找到通路，那就看其他的api
                                        continue;
                                    }
                                };

                                //下面是找到了通路
                                param_reverse_sequences.push(param_seq.clone());

                                //根据dependency_index找到对应的dependency
                                let dependency_ = self.api_dependencies[dependency_index].clone();

                                //将覆盖到的边加入到新的sequence中去
                                //好像没啥用
                                new_reverse_sequence._add_dependency(dependency_index);

                                //找到了依赖，当前参数是可以被满足的，设置flag并退出循环
                                dependency_flag = true;

                                //参数需要加mut 标记的话
                                if api_util::_need_mut_tag(&dependency_.call_type) {
                                    new_reverse_sequence
                                        ._insert_function_mut_tag(current_param_index);
                                }
                                //如果call type是unsafe的，那么给sequence加上unsafe标记
                                if dependency_.call_type.unsafe_call_type()._is_unsafe() {
                                    new_reverse_sequence.set_unsafe();
                                }

                                //为api_call添加依赖
                                api_call._add_param(
                                    ParamType::_FunctionReturn,
                                    current_param_index,
                                    dependency_.call_type,
                                );
                                current_param_index += param_seq.functions.len();

                                println!(
                                    "找到了依赖，{}的返回值给{}",
                                    self.api_functions[output_fun_index].full_name,
                                    self.api_functions[input_fun_index].full_name
                                );
                                break;
                            }
                        }
                        //如果所有函数都无法作为当前函数的前驱。。。
                        if !dependency_flag {
                            println!("所有函数都无法作为当前函数的前驱");
                            return None;
                        }
                    }
                    /******************************************************************************************************** */
                }
                //遍历完所有参数，merge所有反向序列

                new_reverse_sequence.functions.push(api_call);

                for seq in param_reverse_sequences {
                    new_reverse_sequence = new_reverse_sequence.combine(seq);
                }

                if print {
                    new_reverse_sequence.print_reverse_sequence(&self);

                    println!("反向构造结束");
                }
                return Some(new_reverse_sequence);
            }
            ApiType::GenericFunction => todo!(),
        }
    }

    //判断一个依赖是否存在,存在的话返回Some(ApiDependency),否则返回None
    pub(crate) fn check_dependency(
        &self,
        output_type: &ApiType,
        output_index: usize,
        input_type: &ApiType,
        input_index: usize,
        input_param_index_: usize,
    ) -> Option<usize> {
        let dependency_num = self.api_dependencies.len();
        for index in 0..dependency_num {
            let dependency = &self.api_dependencies[index];
            //FIXME: 直接比较每一项内容是否可以节省点时间？
            let tmp_dependency = ApiDependency {
                output_fun: (*output_type, output_index),
                input_fun: (*input_type, input_index),
                input_param_index: input_param_index_,
                call_type: dependency.call_type.clone(),
            };
            if tmp_dependency == *dependency {
                //存在依赖
                return Some(index);
            }
        }
        //没找到依赖
        return None;
    }

    //判断一个调用序列是否已经到达终止端点
    fn is_sequence_ended(&self, api_sequence: &ApiSequence) -> bool {
        let functions = &api_sequence.functions;
        let last_fun = functions.last();
        match last_fun {
            None => false,
            Some(api_call) => {
                let (api_type, index) = &api_call.func;
                match api_type {
                    ApiType::BareFunction => {
                        let last_func = &self.api_functions[*index];
                        if last_func._is_end_function(self.cache, &self.full_name_map) {
                            return true;
                        } else {
                            return false;
                        }
                    }
                    ApiType::GenericFunction => todo!(),
                }
            }
        }
    }
}
